/*
 * panelprobe -- reMarkable Paper Pro (imx8mm-ferrari / cumulus-panel) display probe.
 *
 * WWW-20 Gate 1: establish whether a plain DRM/KMS modeset can put visible pixels
 * on the panel, and if so which framebuffer packing corresponds to panel pixels.
 *
 * Raw DRM ioctls only -- no libdrm, no Qt. Static aarch64 binary.
 *
 * Safety contract:
 *   - Saves the CRTC state before touching it and restores it on EVERY exit path,
 *     including signals and the watchdog alarm.
 *   - Hard watchdog (--watchdog, default 240s) guarantees the display is released.
 *   - Never writes to any device other than /dev/dri/card0.
 */
#define _GNU_SOURCE
#include <drm/drm.h>
#include <drm/drm_mode.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

/* Panel geometry, from the device tree and WWW-1's survey. */
#define PANEL_W 1620
#define PANEL_H 2160

/* Candidate packings: 1620 fb bytes/row = 3240 nibbles = two panel rows of 1620. */
enum packing {
    PACK_ROWPAIR = 1, /* nibbles 0..1619 -> panel row 2r ; 1620..3239 -> row 2r+1 */
    PACK_HALVES  = 2, /* nibbles 0..1619 -> panel row r   ; 1620..3239 -> row r+1084 */
    PACK_INTERLV = 3, /* nibble 2k -> row 2r col k ; nibble 2k+1 -> row 2r+1 col k */
};

static int drm_fd = -1;
static uint8_t *fb_map;
static size_t fb_size;
static uint32_t fb_pitch, fb_w, fb_h;
static uint32_t fb_id, dumb_handle;
static struct drm_mode_crtc saved_crtc;
static int have_saved_crtc;
static int master_held;
static volatile sig_atomic_t restoring;

static void logf_(const char *fmt, ...)
{
    struct timespec ts;
    struct tm tm;
    char when[32];
    va_list ap;

    clock_gettime(CLOCK_REALTIME, &ts);
    localtime_r(&ts.tv_sec, &tm);
    strftime(when, sizeof when, "%H:%M:%S", &tm);
    fprintf(stderr, "[%s.%03ld] ", when, ts.tv_nsec / 1000000);
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fputc('\n', stderr);
    fflush(stderr);
}

static int xioctl(unsigned long req, void *arg)
{
    int r;
    do {
        r = ioctl(drm_fd, req, arg);
    } while (r == -1 && errno == EINTR);
    return r;
}

/* Put the display back exactly as it was found. Safe to call more than once. */
static void restore_display(void)
{
    if (restoring)
        return;
    restoring = 1;

    if (drm_fd >= 0 && have_saved_crtc) {
        struct drm_mode_crtc c = saved_crtc;
        c.set_connectors_ptr = 0;
        c.count_connectors = 0;
        if (xioctl(DRM_IOCTL_MODE_SETCRTC, &c) == 0)
            logf_("restore: CRTC %u returned to saved state (fb=%u mode_valid=%u)",
                  c.crtc_id, c.fb_id, c.mode_valid);
        else
            logf_("restore: SETCRTC failed: %s", strerror(errno));
    }
    if (fb_map && fb_map != MAP_FAILED)
        munmap(fb_map, fb_size);
    if (drm_fd >= 0 && fb_id) {
        uint32_t id = fb_id;
        xioctl(DRM_IOCTL_MODE_RMFB, &id);
    }
    if (drm_fd >= 0 && dumb_handle) {
        struct drm_mode_destroy_dumb d = { .handle = dumb_handle };
        xioctl(DRM_IOCTL_MODE_DESTROY_DUMB, &d);
    }
    if (drm_fd >= 0 && master_held) {
        if (xioctl(DRM_IOCTL_DROP_MASTER, NULL) == 0)
            logf_("restore: DRM master dropped");
        else
            logf_("restore: DROP_MASTER failed: %s", strerror(errno));
    }
    if (drm_fd >= 0)
        close(drm_fd);
    drm_fd = -1;
    logf_("restore: complete");
}

static void on_signal(int sig)
{
    logf_("signal %d received -- restoring display", sig);
    restore_display();
    _exit(sig == SIGALRM ? 3 : 2);
}

static void die(const char *what)
{
    logf_("FATAL: %s: %s", what, strerror(errno));
    restore_display();
    exit(1);
}

/* ---- pixel plotting under a packing hypothesis ---------------------------- */

static enum packing cur_pack = PACK_ROWPAIR;

static inline void put_px(int x, int y, unsigned v)
{
    unsigned row, half, idx;

    if (x < 0 || y < 0 || x >= PANEL_W || y >= PANEL_H)
        return;

    switch (cur_pack) {
    case PACK_HALVES:
        row = (unsigned)y % fb_h;
        half = (unsigned)y / fb_h;
        idx = half * PANEL_W + (unsigned)x;
        break;
    case PACK_INTERLV:
        row = (unsigned)y / 2;
        half = (unsigned)y & 1u;
        idx = (unsigned)x * 2 + half;
        break;
    case PACK_ROWPAIR:
    default:
        row = (unsigned)y / 2;
        half = (unsigned)y & 1u;
        idx = half * PANEL_W + (unsigned)x;
        break;
    }
    if (row >= fb_h)
        return;

    uint8_t *b = &fb_map[row * fb_pitch + idx / 2];
    if (idx & 1u)
        *b = (uint8_t)((*b & 0x0f) | ((v & 0x0f) << 4));
    else
        *b = (uint8_t)((*b & 0xf0) | (v & 0x0f));
}

static void fill_panel(unsigned v)
{
    memset(fb_map, (int)((v & 0x0f) | ((v & 0x0f) << 4)), fb_size);
}

static void rect(int x0, int y0, int w, int h, unsigned v)
{
    for (int y = y0; y < y0 + h; y++)
        for (int x = x0; x < x0 + w; x++)
            put_px(x, y, v);
}

static void disc(int cx, int cy, int r, unsigned v)
{
    for (int y = -r; y <= r; y++)
        for (int x = -r; x <= r; x++)
            if (x * x + y * y <= r * r)
                put_px(cx + x, cy + y, v);
}

static void cross(int cx, int cy, int arm, int thick, unsigned v)
{
    rect(cx - arm, cy - thick / 2, arm * 2, thick, v);
    rect(cx - thick / 2, cy - arm, thick, arm * 2, v);
}

/* ---- scenes -------------------------------------------------------------- */

static void hold(double seconds);

#define WHITE 0x0f
#define BLACK 0x00

/* Packing-independent: 16 bands of a constant byte value.
 * A constant byte is the same under every hypothesis, so this scene tests
 * "is anything visible at all", the grey/colour levels, and the polarity. */
static void scene_ramp(void)
{
    uint32_t band = fb_h / 16;
    for (unsigned i = 0; i < 16; i++) {
        uint8_t v = (uint8_t)(i | (i << 4));
        uint32_t y0 = i * band;
        uint32_t y1 = (i == 15) ? fb_h : y0 + band;
        for (uint32_t y = y0; y < y1; y++)
            memset(&fb_map[y * fb_pitch], v, fb_pitch);
    }
}

/* Packing-independent (coarse in both axes): 8 x 10 blocks in framebuffer space. */
static void scene_checker(void)
{
    uint32_t bw = fb_pitch / 8, bh = fb_h / 10;
    for (uint32_t y = 0; y < fb_h; y++) {
        uint32_t br = y / bh;
        for (uint32_t bx = 0; bx < 8; bx++) {
            uint32_t x0 = bx * bw;
            uint32_t len = (bx == 7) ? fb_pitch - x0 : bw;
            memset(&fb_map[y * fb_pitch + x0], ((br + bx) & 1u) ? 0xff : 0x00, len);
        }
    }
}

/* Packing-dependent: an asymmetric figure drawn in PANEL coordinates.
 * Under the correct hypothesis this is clean and legible; under a wrong one it
 * is halved, duplicated or split across the screen. */
static void scene_geometry(void)
{
    fill_panel(WHITE);
    /* 20px black border */
    rect(0, 0, PANEL_W, 20, BLACK);
    rect(0, PANEL_H - 20, PANEL_W, 20, BLACK);
    rect(0, 0, 20, PANEL_H, BLACK);
    rect(PANEL_W - 20, 0, 20, PANEL_H, BLACK);
    /* top-left: large solid square */
    rect(80, 80, 400, 400, BLACK);
    /* top-right: solid disc */
    disc(PANEL_W - 280, 280, 200, BLACK);
    /* bottom-left: small square (quarter the area of the top-left one) */
    rect(80, PANEL_H - 280, 200, 200, BLACK);
    /* bottom-right: deliberately empty */
    /* mid-screen thin crosshair: tests whether single panel rows survive */
    rect(0, PANEL_H / 2 - 1, PANEL_W, 2, BLACK);
    rect(PANEL_W / 2 - 1, 0, 2, PANEL_H, BLACK);
    /* a 6-step grey wedge down the right side, in panel space */
    for (int i = 0; i < 6; i++)
        rect(PANEL_W - 200, 700 + i * 120, 160, 100, (unsigned)(i * 3));
}

/* Drive the panel through full-field inversions slowly enough for the waveform
 * to complete, to flush retained charge. WWW-20 photographed probe residue
 * still visible over a fully repainted stock UI, so releasing the display
 * without this leaves the user looking at our ghosts. */
static void clear_panel(int cycles, double per_state)
{
    logf_("CLEAR: %d black/white cycles at %.0fms per state", cycles,
          per_state * 1000.0);
    for (int i = 0; i < cycles; i++) {
        fill_panel(BLACK);
        hold(per_state);
        fill_panel(WHITE);
        hold(per_state);
    }
    fill_panel(WHITE);
    hold(per_state);
    logf_("CLEAR: done");
}

/* Three stacked zones, drawn in PANEL coordinates. Each candidate packing
 * turns this into a different, easily described picture:
 *   ROWPAIR      three zones, once: black third, white third, ~7 wide stripes
 *   HALVES       the whole three-zone pattern repeated TWICE down the screen
 *   INTERLEAVED  three zones, but the stripes doubled in count and half as wide
 * Coarse in both axes, so it survives an imperfect panel far better than the
 * ornate figure did. */
static void scene_zones(void)
{
    fill_panel(WHITE);
    rect(0, 0, PANEL_W, 720, BLACK);
    for (int x = 0; x + 120 <= PANEL_W; x += 240)
        rect(x, 1440, 120, 720, BLACK);
}

/* Five numbered fiducial crosses at known panel coordinates (Gate 2). */
static const int FID[5][2] = {
    { 162, 216 }, { 1458, 216 }, { 810, 1080 }, { 162, 1944 }, { 1458, 1944 },
};

static void scene_fiducials(void)
{
    fill_panel(WHITE);
    rect(0, 0, PANEL_W, 8, BLACK);
    rect(0, PANEL_H - 8, PANEL_W, 8, BLACK);
    rect(0, 0, 8, PANEL_H, BLACK);
    rect(PANEL_W - 8, 0, 8, PANEL_H, BLACK);
    for (int i = 0; i < 5; i++) {
        cross(FID[i][0], FID[i][1], 120, 10, BLACK);
        /* i+1 identifying dots, in a row below the cross */
        for (int d = 0; d <= i; d++)
            disc(FID[i][0] - 60 + d * 30, FID[i][1] + 170, 11, BLACK);
    }
}

/* ---- main --------------------------------------------------------------- */

static void hold(double seconds)
{
    struct timespec ts;
    ts.tv_sec = (time_t)seconds;
    ts.tv_nsec = (long)((seconds - (double)ts.tv_sec) * 1e9);
    while (nanosleep(&ts, &ts) == -1 && errno == EINTR)
        ;
}

static uint32_t chosen_connector, chosen_crtc;
static struct drm_mode_modeinfo chosen_mode;

static void pick_output(void)
{
    struct drm_mode_card_res res = { 0 };
    uint64_t fbs[8], crtcs[8], conns[16], encs[16];

    if (xioctl(DRM_IOCTL_MODE_GETRESOURCES, &res) < 0)
        die("GETRESOURCES(count)");
    if (res.count_crtcs > 8 || res.count_connectors > 16)
        die("unexpected resource counts");

    res.fb_id_ptr = (uint64_t)(uintptr_t)fbs;
    res.crtc_id_ptr = (uint64_t)(uintptr_t)crtcs;
    res.connector_id_ptr = (uint64_t)(uintptr_t)conns;
    res.encoder_id_ptr = (uint64_t)(uintptr_t)encs;
    if (res.count_fbs > 8) res.count_fbs = 8;
    if (xioctl(DRM_IOCTL_MODE_GETRESOURCES, &res) < 0)
        die("GETRESOURCES");
    logf_("resources: %u crtc, %u connector, %u encoder",
          res.count_crtcs, res.count_connectors, res.count_encoders);

    for (uint32_t i = 0; i < res.count_connectors; i++) {
        struct drm_mode_get_connector gc = { 0 };
        struct drm_mode_modeinfo modes[32];
        uint64_t props[64], pvals[64], cencs[8];

        gc.connector_id = (uint32_t)((uint32_t *)conns)[i];
        if (xioctl(DRM_IOCTL_MODE_GETCONNECTOR, &gc) < 0)
            continue;
        if (gc.count_modes > 32) gc.count_modes = 32;
        if (gc.count_props > 64) gc.count_props = 64;
        if (gc.count_encoders > 8) gc.count_encoders = 8;
        gc.modes_ptr = (uint64_t)(uintptr_t)modes;
        gc.props_ptr = (uint64_t)(uintptr_t)props;
        gc.prop_values_ptr = (uint64_t)(uintptr_t)pvals;
        gc.encoders_ptr = (uint64_t)(uintptr_t)cencs;
        if (xioctl(DRM_IOCTL_MODE_GETCONNECTOR, &gc) < 0)
            continue;

        logf_("connector %u: type=%u connection=%u modes=%u %umm x %umm",
              gc.connector_id, gc.connector_type, gc.connection, gc.count_modes,
              gc.mm_width, gc.mm_height);
        if (gc.connection != 1 || gc.count_modes == 0)
            continue;
        for (uint32_t m = 0; m < gc.count_modes; m++)
            logf_("  mode[%u] %ux%u @%u vrefresh=%u clock=%u",
                  m, modes[m].hdisplay, modes[m].vdisplay, modes[m].vrefresh,
                  modes[m].vrefresh, modes[m].clock);

        chosen_connector = gc.connector_id;
        chosen_mode = modes[0];

        /* find a usable CRTC via this connector's encoder */
        if (gc.encoder_id) {
            struct drm_mode_get_encoder ge = { .encoder_id = gc.encoder_id };
            if (xioctl(DRM_IOCTL_MODE_GETENCODER, &ge) == 0 && ge.crtc_id)
                chosen_crtc = ge.crtc_id;
        }
        if (!chosen_crtc && res.count_crtcs)
            chosen_crtc = ((uint32_t *)crtcs)[0];
        break;
    }
    if (!chosen_connector || !chosen_crtc)
        die("no connected output with a mode");
    logf_("using connector %u, crtc %u, mode %ux%u",
          chosen_connector, chosen_crtc, chosen_mode.hdisplay, chosen_mode.vdisplay);
}

static void set_crtc_to_fb(void)
{
    struct drm_mode_crtc sc = { 0 };
    uint32_t conn = chosen_connector;

    sc.crtc_id = chosen_crtc;
    sc.fb_id = fb_id;
    sc.x = sc.y = 0;
    sc.set_connectors_ptr = (uint64_t)(uintptr_t)&conn;
    sc.count_connectors = 1;
    sc.mode = chosen_mode;
    sc.mode_valid = 1;
    if (xioctl(DRM_IOCTL_MODE_SETCRTC, &sc) < 0)
        die("SETCRTC");
    logf_("SETCRTC accepted: crtc %u scanning fb %u (%ux%u)",
          chosen_crtc, fb_id, chosen_mode.hdisplay, chosen_mode.vdisplay);
}

int main(int argc, char **argv)
{
    unsigned watchdog = 240;
    double t_scene = 10.0, t_flash = 6.0;
    int do_fid = 0;
    double t_hold = 0.0;   /* --hold S: single scene, held with a ticker */
    enum packing hold_pack = PACK_ROWPAIR;   /* --pack */
    int hold_fid = 0;                        /* --hold-fiducials */
    int hold_zones = 0;                      /* --hold-zones */
    int hold_clear = 1;                      /* --no-clear disables */

    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--watchdog") && i + 1 < argc)
            watchdog = (unsigned)atoi(argv[++i]);
        else if (!strcmp(argv[i], "--scene-seconds") && i + 1 < argc)
            t_scene = atof(argv[++i]);
        else if (!strcmp(argv[i], "--fiducials"))
            do_fid = 1;
        else if (!strcmp(argv[i], "--hold") && i + 1 < argc)
            t_hold = atof(argv[++i]);
        else if (!strcmp(argv[i], "--hold-fiducials"))
            hold_fid = 1;
        else if (!strcmp(argv[i], "--hold-zones"))
            hold_zones = 1;
        else if (!strcmp(argv[i], "--no-clear"))
            hold_clear = 0;
        else if (!strcmp(argv[i], "--pack") && i + 1 < argc) {
            const char *v = argv[++i];
            if (!strcmp(v, "rowpair")) hold_pack = PACK_ROWPAIR;
            else if (!strcmp(v, "halves")) hold_pack = PACK_HALVES;
            else if (!strcmp(v, "interleaved")) hold_pack = PACK_INTERLV;
            else { fprintf(stderr, "unknown --pack %s\n", v); return 64; }
        }
        else {
            fprintf(stderr, "usage: %s [--watchdog S] [--scene-seconds S] [--fiducials] [--hold S] [--pack rowpair|halves|interleaved] [--hold-fiducials] [--hold-zones] [--no-clear]\n", argv[0]);
            return 64;
        }
    }

    struct sigaction sa = { 0 };
    sa.sa_handler = on_signal;
    sigaction(SIGINT, &sa, NULL);
    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGHUP, &sa, NULL);
    sigaction(SIGALRM, &sa, NULL);
    sigaction(SIGPIPE, &sa, NULL);
    alarm(watchdog);
    logf_("watchdog armed at %u seconds", watchdog);

    drm_fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    if (drm_fd < 0)
        die("open /dev/dri/card0");

    if (xioctl(DRM_IOCTL_SET_MASTER, NULL) < 0) {
        logf_("SET_MASTER refused: %s -- is xochitl still running?", strerror(errno));
        restore_display();
        return 10;
    }
    master_held = 1;
    logf_("SET_MASTER granted");

    pick_output();

    saved_crtc.crtc_id = chosen_crtc;
    if (xioctl(DRM_IOCTL_MODE_GETCRTC, &saved_crtc) < 0)
        die("GETCRTC");
    have_saved_crtc = 1;
    logf_("saved CRTC state: fb=%u mode_valid=%u x=%u y=%u",
          saved_crtc.fb_id, saved_crtc.mode_valid, saved_crtc.x, saved_crtc.y);

    struct drm_mode_create_dumb cd = { 0 };
    cd.width = chosen_mode.hdisplay;
    cd.height = chosen_mode.vdisplay;
    cd.bpp = 32;
    if (xioctl(DRM_IOCTL_MODE_CREATE_DUMB, &cd) < 0)
        die("CREATE_DUMB");
    dumb_handle = cd.handle;
    fb_pitch = cd.pitch;
    fb_w = cd.width;
    fb_h = cd.height;
    fb_size = cd.size;
    logf_("CREATE_DUMB: %ux%u bpp=32 pitch=%u size=%llu",
          cd.width, cd.height, cd.pitch, (unsigned long long)cd.size);
    logf_("panel model: %ux%u at 4bpp -> %u bytes/row needed, fb provides %u",
          PANEL_W, PANEL_H, PANEL_W / 2 * 2, fb_pitch);

    struct drm_mode_fb_cmd fb = { 0 };
    fb.width = cd.width;
    fb.height = cd.height;
    fb.pitch = cd.pitch;
    fb.bpp = 32;
    fb.depth = 24;
    fb.handle = cd.handle;
    if (xioctl(DRM_IOCTL_MODE_ADDFB, &fb) < 0)
        die("ADDFB");
    fb_id = fb.fb_id;
    logf_("ADDFB: fb_id=%u", fb_id);

    struct drm_mode_map_dumb md = { .handle = cd.handle };
    if (xioctl(DRM_IOCTL_MODE_MAP_DUMB, &md) < 0)
        die("MAP_DUMB");
    fb_map = mmap(NULL, fb_size, PROT_READ | PROT_WRITE, MAP_SHARED, drm_fd,
                  (off_t)md.offset);
    if (fb_map == MAP_FAILED)
        die("mmap");
    logf_("mmap ok at %p", (void *)fb_map);

    /* Start from all-black so the first transition is unambiguous. */
    fill_panel(BLACK);
    set_crtc_to_fb();

    if (t_hold > 0.0) {
        /* Single-scene hold with a one-second ticker. Used to observe what
         * happens to a custom display session across a suspend/resume cycle:
         * if the ticker keeps advancing afterwards the process survived, and
         * the wrapper's sysfs sampler says whether the panel stayed powered. */
        cur_pack = hold_pack;
        if (hold_clear)
            clear_panel(6, 0.4);
        if (hold_fid)
            scene_fiducials();
        else if (hold_zones)
            scene_zones();
        else
            scene_geometry();
        logf_("HOLD: %s(packing=%s) held for %.0fs with 1s ticker",
              hold_fid ? "fiducials" : hold_zones ? "zones" : "geometry",
              hold_pack == PACK_ROWPAIR ? "ROWPAIR" :
              hold_pack == PACK_HALVES ? "HALVES" : "INTERLEAVED", t_hold);
        for (int t = 0; (double)t < t_hold; t++) {
            /* advance a black square across the middle band, one step per second */
            rect(40 + ((t - 1) % 30) * 50, 1150, 40, 30, WHITE);
            rect(40 + (t % 30) * 50, 1150, 40, 30, BLACK);
            logf_("HOLD tick %d", t);
            hold(1.0);
        }
        logf_("HOLD complete");
        if (hold_clear)
            clear_panel(6, 0.4);
        restore_display();
        return 0;
    }

    /* Scene 1 -- FLASH. Packing-independent, and the only scene that supplies a
     * changing image, which is what an EPD needs if the FPGA bridge expects the
     * host to drive the waveform rather than generating it itself. */
    logf_("SCENE 1/6 FLASH: full-field black<->white, ~5Hz, %.0fs", t_flash);
    for (double t = 0; t < t_flash; t += 0.2) {
        fill_panel(BLACK);
        hold(0.1);
        fill_panel(WHITE);
        hold(0.1);
    }

    logf_("SCENE 2/6 RAMP: 16 constant-byte horizontal bands, %.0fs", t_scene);
    scene_ramp();
    hold(t_scene);

    logf_("SCENE 3/6 CHECKER: 8x10 coarse blocks in framebuffer space, %.0fs", t_scene);
    scene_checker();
    hold(t_scene);

    logf_("SCENE 4/6 GEOMETRY packing=ROWPAIR (nibbles 0-1619 = panel row 2r), %.0fs", t_scene);
    cur_pack = PACK_ROWPAIR;
    scene_geometry();
    hold(t_scene);

    logf_("SCENE 5/6 GEOMETRY packing=HALVES (nibbles 0-1619 = panel row r), %.0fs", t_scene);
    cur_pack = PACK_HALVES;
    scene_geometry();
    hold(t_scene);

    logf_("SCENE 6/6 GEOMETRY packing=INTERLEAVED (nibble pairs = two rows), %.0fs", t_scene);
    cur_pack = PACK_INTERLV;
    scene_geometry();
    hold(t_scene);

    if (do_fid) {
        logf_("SCENE 7 FIDUCIALS packing=ROWPAIR, %.0fs", t_scene);
        cur_pack = PACK_ROWPAIR;
        scene_fiducials();
        hold(t_scene);
    }

    logf_("all scenes done");
    restore_display();
    return 0;
}
