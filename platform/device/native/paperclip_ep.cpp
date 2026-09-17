/*
 * The Paperclip side of the vendor waveform engine.
 *
 * ## Status
 *
 * Compiles and links against the real `libqsgepaper.so` on an aarch64 Linux
 * host with Qt 6.10 — `check-link.sh` does that in a container and is the
 * repeatable form of the claim. It has **never run on the tablet**, and the
 * waveform mode numbers below are still guesses. See ADR-0009.
 *
 * Two things that run established, and that no amount of reading the exports
 * would have:
 *
 *   - `EPFramebuffer::instance()` **segfaults without a live
 *     `QCoreApplication`**, and a plain `QCoreApplication` is enough — no
 *     `QGuiApplication`, no QPA platform plugin. So this bridge owns one.
 *   - The engine **`abort()`s** when it cannot initialise, rather than
 *     throwing or returning. `catch (...)` cannot intercept that, so the only
 *     defence is to check its preconditions before calling it — see
 *     `preflight()`.
 *
 * ## Why this is C++ at all
 *
 * The exported interface is C++ with Qt types in its signatures, so it cannot
 * be called from Rust directly. This file is the smallest thing that turns it
 * into C: no Qt type crosses the boundary, no exception crosses it, and the
 * only object that does is an opaque handle.
 *
 * Interface shape follows MaximeRivest/quill (MIT), which wraps the same
 * library for the same reason. No Quill code is copied; see ADR-0009.
 */

#include "paperclip_ep.h"

#include <cstdint>
#include <cstring>
#include <cstdio>
#include <cstdlib>
#include <QtCore/QCoreApplication>
#include <QtCore/QRect>
#include <QtGui/QImage>

#include <algorithm>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <dirent.h>
#include <exception>
#include <new>
#include <pthread.h>
#include <string>
#include <sys/stat.h>
#include <tuple>

#include "ep_abi.hpp"

namespace {

/* Panel geometry. 1620 x 2160 ARGB8888, confirmed from the device tree
 * (WWW-1) and corroborated by rmweb's device profile (ADR-0007). */
constexpr int kPanelWidth = 1620;
constexpr int kPanelHeight = 2160;

/* The engine's SWTCON backend loads a waveform table at construction and calls
 * abort() if it cannot — observed in a container with the directory absent:
 *
 *     /usr/share/remarkable/ct33_std.bin: file does not exist.
 *     Failed to initialize SWTCON.
 *
 * followed by SIGABRT. A catch(...) cannot intercept that, so the only defence
 * is to refuse before instance() is reached.
 *
 * WHICH file it wants is panel-specific and not ours to predict. On Calum's
 * tablet the engine reported `panel tft: C0F` and `panel fpl: AAB0AU` and then
 * loaded
 *
 *     /usr/share/remarkable/GAL3_AAB0AU_ID1C11_AC118TC1F2_AD1004-LHA_TC.eink
 *
 * not any of the `ct33_*.bin` files an earlier revision of this code insisted
 * on. Requiring those by name would wrongly refuse a tablet with a different
 * panel lot. So the check is only that the vendor's waveform directory exists
 * and holds at least one non-empty candidate; the engine does the real
 * selection, which is its job and not ours. */
const char *const kWaveformDir = "/usr/share/remarkable";

/* Per-thread error text. Thread-local so a message cannot be overwritten by
 * another thread between a failing call and the caller reading it — even
 * though the contract says one thread, because a contract violation should
 * produce a wrong-thread error rather than a data race. */
thread_local std::string g_last_error = "";

int32_t fail(int32_t status, const char *message) noexcept
{
    try {
        g_last_error = message;
    } catch (...) {
        /* An allocation failure while recording an error is not worth
         * escalating past the status code the caller is about to get. */
    }
    return status;
}

/* Everything that must be true before EPFramebuffer::instance() is called,
 * because after that point a failure is a process abort rather than an error
 * this bridge can report. Returns PAPERCLIP_EP_OK or a status. */
int32_t preflight() noexcept
{
    if (QCoreApplication::instance() == nullptr) {
        return fail(PAPERCLIP_EP_NOT_OPEN,
                    "no QCoreApplication: EPFramebuffer::instance() segfaults without one");
    }

    DIR *dir = opendir(kWaveformDir);
    if (dir == nullptr) {
        return fail(PAPERCLIP_EP_NOT_OPEN,
                    "no /usr/share/remarkable: the engine aborts rather than failing");
    }
    bool found = false;
    for (const dirent *entry = readdir(dir); entry != nullptr && !found;
         entry = readdir(dir)) {
        const char *dot = strrchr(entry->d_name, '.');
        if (dot == nullptr) {
            continue;
        }
        if (strcmp(dot, ".eink") != 0 && strcmp(dot, ".bin") != 0) {
            continue;
        }
        char path[512];
        if (snprintf(path, sizeof path, "%s/%s", kWaveformDir, entry->d_name)
            >= static_cast<int>(sizeof path)) {
            continue;
        }
        struct stat info = {};
        found = stat(path, &info) == 0 && info.st_size > 0;
    }
    closedir(dir);

    if (!found) {
        return fail(PAPERCLIP_EP_NOT_OPEN,
                    "no waveform table in /usr/share/remarkable: the engine aborts "
                    "rather than failing");
    }
    return PAPERCLIP_EP_OK;
}

} // namespace

struct paperclip_ep {
    EPFramebuffer *engine = nullptr;
    /* Owned only when this bridge created it. A host that already runs Qt
     * keeps its own, and this stays null and is not destroyed. */
    QCoreApplication *owned_app = nullptr;
    /* The bridge owns these for as long as the handle lives. EPFramebuffer is
     * handed the two page buffers and the auxiliary one by setBuffers; Rust
     * borrows `pixels` below and never frees anything. */
    QImage front;
    QImage back;
    QImage aux;
    /* Borrowed, never owned: the engine's own aux QImage, the surface it
     * actually renders from. Lives inside the EPFramebuffer object. */
    QImage *engine_aux = nullptr;
    /* `front`'s pixel address, cached in `open` *before* setBuffers runs — at
     * which point the refcount is still 1, so taking it cannot copy anything
     * and the address is stable for the handle's life.
     *
     * After setBuffers the engine shares this data, and every non-const QImage
     * accessor on `front` would detach it onto fresh memory. That is the
     * WWW-29 bug: the caller keeps drawing, the engine keeps presenting the
     * white it was handed at open, and nothing returns an error. So this is
     * the only pixel address the bridge ever hands out or writes through, and
     * `present` refuses to swap if `front` has moved away from it. */
    uint32_t *pixels = nullptr;
    /* How many uint32_t `pixels` addresses — `stride_pixels * height`. Cached
     * with the pointer because both describe the same allocation and a fill
     * bounded by a re-read could outlive the thing it was measured from. */
    size_t pixel_count = 0;
    pthread_t owner = {};
};

namespace {
/* DIAGNOSTIC (white-panel investigation). PAPERCLIP_EP_DEBUG=1 dumps what the
 * engine object actually contains, so the buffer question is answered by
 * observation instead of by a hardcoded offset nobody can check from here. */
static bool ep_debug() { return ::getenv("PAPERCLIP_EP_DEBUG") != nullptr; }

static std::FILE *ep_log()
{
    static std::FILE *f = std::fopen("/tmp/paperclip-ep-debug.log", "a");
    return f != nullptr ? f : stderr;
}

/* Find the engine's 32-bit drawing surface by looking for it, rather than
 * trusting an offset read off someone else's firmware. The panel-sized image
 * whose bytesPerLine implies 4 bytes per pixel is the one swapBuffers scans
 * out; the Grayscale8 image of the same dimensions is engine-internal. */
static QImage *find_draw_surface(void *engine, int want_w, int want_h)
{
    QImage *best = nullptr;
    /* Only the offsets the disassembly actually names. A blind sweep segfaults:
     * a plausible-looking pointer is not necessarily a QImageData. An explicit
     * PAPERCLIP_AUX_OFFSET overrides, so a new build can be probed without a
     * rebuild. */
    std::ptrdiff_t candidates[] = {0x88, 0xa8, 0xc8};
    if (const char *forced = ::getenv("PAPERCLIP_AUX_OFFSET")) {
        candidates[0] = static_cast<std::ptrdiff_t>(::strtol(forced, nullptr, 0));
        candidates[1] = candidates[0];
        candidates[2] = candidates[0];
    }
    for (std::ptrdiff_t off : candidates) {
        char *base = reinterpret_cast<char *>(engine) + off;
        void *dptr = nullptr;
        ::memcpy(&dptr, base, sizeof(dptr));
        if (dptr == nullptr || reinterpret_cast<uintptr_t>(dptr) < 0x1000
            || (reinterpret_cast<uintptr_t>(dptr) & 0x7) != 0) {
            continue;
        }
        QImage *img = reinterpret_cast<QImage *>(base);
        const int w = img->width();
        const int h = img->height();
        if (w != want_w || h != want_h) {
            continue;
        }
        const long long bpl = static_cast<long long>(img->bytesPerLine());
        if (ep_debug()) {
            std::fprintf(ep_log(), "[ep] candidate +0x%02lx %dx%d fmt=%d bpl=%lld bits=%p\n",
                         static_cast<unsigned long>(off), w, h,
                         static_cast<int>(img->format()), bpl,
                         static_cast<const void *>(img->constBits()));
            std::fflush(ep_log());
        }
        /* Dimensions alone do not identify the surface: the engine holds a
         * Grayscale8 image of exactly the same 1620x2160, and binding to it
         * reports a plausible-looking panel while nothing reaches the glass.
         * The format is what separates them. RGB32/ARGB32/ARGB32_Premultiplied
         * are the 32bpp scanout formats; Grayscale8 (24) is engine-internal. */
        const int fmt = static_cast<int>(img->format());
        const bool is_32bpp = (fmt == 4 || fmt == 5 || fmt == 6);
        const bool stride_agrees = bpl >= static_cast<long long>(want_w) * 4;
        if (is_32bpp && stride_agrees) {
            best = img;
        }
    }
    return best;
}

static void dump_candidate(const char *label, void *engine, std::ptrdiff_t off)
{
    char *base = reinterpret_cast<char *>(engine) + off;
    /* A QImage is one d-pointer. Refuse to touch anything that cannot be one,
     * so a bad offset prints "implausible" rather than killing the process. */
    void *dptr = nullptr;
    ::memcpy(&dptr, base, sizeof(dptr));
    if (dptr == nullptr || reinterpret_cast<uintptr_t>(dptr) < 0x1000
        || (reinterpret_cast<uintptr_t>(dptr) & 0x7) != 0) {
        std::fprintf(ep_log(), "  +0x%02lx %-12s d=%p implausible, not probed\n",
                     static_cast<unsigned long>(off), label, dptr);
        return;
    }
    const QImage *img = reinterpret_cast<const QImage *>(base);
    const int w = img->width();
    const int h = img->height();
    const int fmt = static_cast<int>(img->format());
    const qsizetype bpl = img->bytesPerLine();
    const uchar *bits = img->constBits();
    std::fprintf(ep_log(),
                 "  +0x%02lx %-12s d=%p  %dx%d fmt=%d bpl=%lld bits=%p\n",
                 static_cast<unsigned long>(off), label, dptr, w, h, fmt,
                 static_cast<long long>(bpl), static_cast<const void *>(bits));
}

static void dump_engine(void *engine)
{
    std::fprintf(ep_log(), "[ep] engine=%p  candidate QImages:\n", engine);
    dump_candidate("main?", engine, 0x88);
    dump_candidate("aux?", engine, 0xa8);
    dump_candidate("alt?", engine, 0xc8);
    std::fprintf(ep_log(), "[ep] first 0x110 bytes of the engine object:\n");
    const unsigned char *raw = reinterpret_cast<const unsigned char *>(engine);
    for (int row = 0; row < 0x110; row += 16) {
        std::fprintf(ep_log(), "  %04x:", row);
        for (int i = 0; i < 16; ++i) {
            std::fprintf(ep_log(), " %02x", raw[row + i]);
        }
        std::fprintf(ep_log(), "\n");
    }
    std::fflush(ep_log());
}

/* Offset of the aux QImage inside EPFramebufferAcep2 on this image.
 * swapBuffers_impl derives the panel rect from the QImage at this+0xa8. */
static constexpr std::ptrdiff_t kAuxBufferOffset = 0xa8;
} // namespace

extern "C" {

uint32_t paperclip_ep_abi_version(void)
{
    return PAPERCLIP_EP_ABI_VERSION;
}

const char *paperclip_ep_last_error(void)
{
    return g_last_error.c_str();
}

int32_t paperclip_ep_open(paperclip_ep **out)
{
    if (out == nullptr) {
        return fail(PAPERCLIP_EP_INVALID_ARGUMENT, "paperclip_ep_open: out is null");
    }
    *out = nullptr;

    paperclip_ep *handle = nullptr;
    try {
        handle = new (std::nothrow) paperclip_ep();
        if (handle == nullptr) {
            return fail(PAPERCLIP_EP_OUT_OF_MEMORY, "allocating the bridge handle failed");
        }

        /* A QCoreApplication must exist and must outlive the engine. Creating
         * it here rather than asking the caller to is the whole point of this
         * bridge: Paperclip's host is a Rust process with no Qt in it. */
        if (QCoreApplication::instance() == nullptr) {
            /* QCoreApplication keeps a reference to argc and argv for its
             * entire lifetime, so these must outlive it. Static storage is the
             * only way to guarantee that without making destruction order
             * load-bearing. */
            static char program_name[] = "paperclip";
            static char *argv[] = {program_name, nullptr};
            static int argc = 1;
            handle->owned_app = new (std::nothrow) QCoreApplication(argc, argv);
            if (handle->owned_app == nullptr) {
                delete handle;
                return fail(PAPERCLIP_EP_OUT_OF_MEMORY, "allocating QCoreApplication failed");
            }
        }

        const int32_t ready = preflight();
        if (ready != PAPERCLIP_EP_OK) {
            delete handle;
            return ready;
        }

        EPFramebuffer *engine = EPFramebuffer::instance();
        if (engine == nullptr) {
            delete handle;
            return fail(PAPERCLIP_EP_NOT_OPEN, "EPFramebuffer::instance() returned null");
        }
        /* The vendor's own side of the advisory registry Xochitl participates
         * in. Holding DRM master is not display ownership on this device. */
        if (!engine->checkLockFile()) {
            delete handle;
            return fail(PAPERCLIP_EP_LOCKED,
                        "EPFramebuffer::checkLockFile() reports another instance");
        }

        handle->engine = engine;
        handle->owner = pthread_self();
        handle->front = QImage(kPanelWidth, kPanelHeight, QImage::Format_ARGB32);
        handle->back = QImage(kPanelWidth, kPanelHeight, QImage::Format_ARGB32);
        handle->aux = QImage(kPanelWidth, kPanelHeight, QImage::Format_ARGB32);
        if (handle->front.isNull() || handle->back.isNull() || handle->aux.isNull()) {
            delete handle;
            return fail(PAPERCLIP_EP_OUT_OF_MEMORY, "allocating the panel buffers failed");
        }
        handle->front.fill(0xFFFFFFFFu);
        handle->back.fill(0xFFFFFFFFu);
        handle->aux.fill(0xFFFFFFFFu);

        /* Cache the pixel address HERE, before setBuffers. This is the only
         * moment `front` is unshared, so `bits()` cannot copy and the address
         * it returns is the one the engine is about to start sharing. Taking
         * it any later is the WWW-29 bug. */
        /* The engine renders from ITS OWN aux QImage, allocated during
         * EPFramebuffer::instance(). setBuffers is an engine-internal init call
         * — handing it our images does nothing, which is why every present so
         * far painted the engine's blank buffer white. */
        handle->engine_aux = find_draw_surface(engine, kPanelWidth, kPanelHeight);
        if (handle->engine_aux == nullptr) {
            delete handle;
            return fail(PAPERCLIP_EP_NOT_OPEN,
                        "no panel-sized 32bpp QImage found inside the engine object");
        }
        if (ep_debug()) {
            dump_engine(engine);
        }
        /* constBits(), NEVER bits(): the engine's own QImage is shared
         * internally, so the non-const accessor would detach it onto fresh
         * memory — we would draw into a private copy while the engine kept
         * presenting the white original. Same failure as WWW-29, on the far
         * side of the boundary. Writing through the const pointer is
         * deliberate: this memory is the engine's scanout surface. */
        handle->pixels = const_cast<uint32_t *>(
            reinterpret_cast<const uint32_t *>(handle->engine_aux->constBits()));
        if (handle->pixels == nullptr) {
            delete handle;
            return fail(PAPERCLIP_EP_OUT_OF_MEMORY, "the engine's aux buffer has no pixels");
        }
        handle->pixel_count = static_cast<size_t>(handle->engine_aux->bytesPerLine() / 4)
            * static_cast<size_t>(handle->engine_aux->height());

        /* The engine presents from `front`, the first element of the tuple,
         * which is also what `paperclip_ep_buffer` hands out.
         *
         * ESTABLISHED BY DISASSEMBLY, WWW-29 — not by readback. `setBuffers`
         * at 0x328c0 stores `std::get<0>(tuple)` into `this+0x88`, and
         * `swapBuffers` at 0x32930 reads `this+0x88`. It stores by
         * `QImage::operator=`, so what it keeps *shares* our pixel data rather
         * than copying it; that sharing is the transport, and severing it is
         * the failure `PAPERCLIP_EP_DETACHED` exists to catch.
         *
         * WWW-23 reached the same conclusion from a readback and was not
         * entitled to: at the time `paperclip_ep_buffer` had already detached
         * `front` onto private memory, so "front came back byte-identical to
         * what was sent" was this side's buffer agreeing with itself and would
         * have read the same however the engine behaved. The conclusion
         * survived; the evidence for it did not. ADR-0009 records both. */
        /* Deliberately NOT calling setBuffers: it is the engine's own init
         * call and our images are not what it renders from. */

        *out = handle;
        g_last_error.clear();
        return PAPERCLIP_EP_OK;
    } catch (const std::exception &error) {
        delete handle;
        return fail(PAPERCLIP_EP_EXCEPTION, error.what());
    } catch (...) {
        delete handle;
        return fail(PAPERCLIP_EP_EXCEPTION, "paperclip_ep_open: unknown C++ exception");
    }
}

void paperclip_ep_close(paperclip_ep *ep)
{
    if (ep == nullptr) {
        return;
    }
    try {
        /* Order matters: the engine's buffers are QImages, so they go before
         * the QCoreApplication that Qt's own teardown depends on. */
        QCoreApplication *app = ep->owned_app;
        ep->owned_app = nullptr;
        delete ep;
        delete app;
    } catch (...) {
        /* Nothing useful can be done and nothing may propagate: this runs on
         * the shutdown path, including the one after a failure. */
    }
}

} // extern "C"

namespace {

int32_t check(paperclip_ep *ep) noexcept
{
    if (ep == nullptr || ep->engine == nullptr) {
        return fail(PAPERCLIP_EP_NOT_OPEN, "the waveform engine is not open");
    }
    if (pthread_equal(ep->owner, pthread_self()) == 0) {
        return fail(PAPERCLIP_EP_WRONG_THREAD,
                    "called from a thread other than the one that opened the engine");
    }
    return PAPERCLIP_EP_OK;
}

/* Whether `front` still shares the memory the caller has been drawing into.
 *
 * Qt detaches silently, so nothing else in this file would notice. A detach
 * means the frame the caller drew is not the frame the engine holds, and
 * presenting anyway would report success over a stale page — precisely the
 * failure that hid WWW-29 for four issues. It is an error, never a present. */
int32_t attached(paperclip_ep *ep) noexcept
{
    if (ep->pixels == nullptr) {
        /* Not reachable through a handle `open` returned — it fails rather
         * than hand back a handle with no cached address — so this is the
         * absence of a buffer, not a detached one. */
        return fail(PAPERCLIP_EP_NOT_OPEN, "the front buffer was never cached");
    }
    if (ep->engine_aux == nullptr
        || ep->engine_aux->constBits() != reinterpret_cast<const uchar *>(ep->pixels)) {
        return fail(PAPERCLIP_EP_DETACHED,
                    "the front buffer detached from the engine's: the frame that was drawn "
                    "is not the frame the engine holds");
    }
    return PAPERCLIP_EP_OK;
}

/* EPScreenMode is the mode number on its own.
 *
 * An earlier reading packed the content type into bit 8 — `0x100 | mode` — and
 * the device refuted it in one run: a colour swap printed
 *
 *     Invalid screen mode being set: 260
 *
 * which is `0x100 | 4`, and the swap returned in 190ms against the 2181ms a
 * real full-panel update took. So the engine rejected it outright.
 *
 * Mono and colour share one numbering — mono 0 and 3, colour 3/4/5 — so the
 * content type is not part of this call at all. It stays in the C ABI because
 * it is real information the Rust side carries, and because the vendor has a
 * separate notion of content type that a later revision may need to pass
 * somewhere else; it simply does not belong here. */

int32_t present(paperclip_ep *ep, QRect rect, int32_t content, int32_t mode,
                int32_t full) noexcept
{
    (void)content;
    const int32_t shared = attached(ep);
    if (shared != PAPERCLIP_EP_OK) {
        return shared;
    }
    try {
        const int screen_mode = mode;
        const int flags = full != 0 ? 1 : 0;
        /* DIAGNOSTIC PROBE (WWW-?? white-panel investigation). With
         * PAPERCLIP_PROBE_FILL=<hex ARGB> set, overwrite the whole buffer with
         * one colour immediately before the swap. A solid fill is the only
         * stimulus that distinguishes "the engine never reads our memory" from
         * "the engine reads it and misinterprets the content" — the Home
         * screen is near-white and cannot tell those apart. Remove once the
         * panel renders. */
        if (const char *probe = ::getenv("PAPERCLIP_PROBE_FILL")) {
            const uint32_t value =
                static_cast<uint32_t>(::strtoul(probe, nullptr, 16));
            for (size_t i = 0; i < ep->pixel_count; ++i) {
                ep->pixels[i] = value;
            }
        }
        if (ep_debug()) {
            std::fprintf(ep_log(),
                         "[ep] writing to %p (%zu px); engine_aux->bits()=%p "
                         "bpl=%lld  first px=%08x mid px=%08x\n",
                         static_cast<void *>(ep->pixels), ep->pixel_count,
                         static_cast<void *>(const_cast<uchar *>(ep->engine_aux->constBits())),
                         static_cast<long long>(ep->engine_aux->bytesPerLine()),
                         ep->pixels[0], ep->pixels[ep->pixel_count / 2]);
            std::fflush(ep_log());
        }
        ep->engine->swapBuffers(
            rect, static_cast<EPScreenMode>(screen_mode),
            QFlags<EPFramebuffer::UpdateFlag>(
                static_cast<EPFramebuffer::UpdateFlag>(flags)));
        return PAPERCLIP_EP_OK;
    } catch (const std::exception &error) {
        return fail(PAPERCLIP_EP_EXCEPTION, error.what());
    } catch (...) {
        return fail(PAPERCLIP_EP_EXCEPTION, "swapBuffers: unknown C++ exception");
    }
}

} // namespace

extern "C" {

int32_t paperclip_ep_geometry(paperclip_ep *ep, int32_t *width, int32_t *height,
                              int32_t *stride_pixels)
{
    const int32_t status = check(ep);
    if (status != PAPERCLIP_EP_OK) {
        return status;
    }
    if (width == nullptr || height == nullptr || stride_pixels == nullptr) {
        return fail(PAPERCLIP_EP_INVALID_ARGUMENT, "paperclip_ep_geometry: null out parameter");
    }
    try {
        *width = ep->engine_aux->width();
        *height = ep->engine_aux->height();
        *stride_pixels = static_cast<int32_t>(ep->engine_aux->bytesPerLine() / 4);
        return PAPERCLIP_EP_OK;
    } catch (...) {
        return fail(PAPERCLIP_EP_EXCEPTION, "paperclip_ep_geometry: unknown C++ exception");
    }
}

uint32_t *paperclip_ep_buffer(paperclip_ep *ep)
{
    if (check(ep) != PAPERCLIP_EP_OK) {
        return nullptr;
    }
    /* The cached address, never `front.bits()`. `bits()` is non-const, the
     * engine shares this data from `open` onwards, and Qt's contract for a
     * non-const accessor on shared data is to detach — so the obvious spelling
     * of this function is the one that orphans every frame. No Qt call here
     * means nothing to throw, which is why there is no try block left. */
    if (ep->pixels == nullptr) {
        fail(PAPERCLIP_EP_NOT_OPEN, "paperclip_ep_buffer: the front buffer was never cached");
        return nullptr;
    }
    return ep->pixels;
}

int32_t paperclip_ep_readback(paperclip_ep *ep, int32_t plane, uint32_t *out, int32_t len)
{
    const int32_t status = check(ep);
    if (status != PAPERCLIP_EP_OK) {
        return status;
    }
    if (out == nullptr || len <= 0) {
        return fail(PAPERCLIP_EP_INVALID_ARGUMENT, "paperclip_ep_readback: no destination");
    }
    try {
        const QImage *image = nullptr;
        switch (plane) {
        case PAPERCLIP_EP_PLANE_FRONT: {
            /* A detached front reads back as this side's private copy, which
             * matches whatever was drawn into it no matter what the engine
             * holds. That vacuous match is the WWW-23 result, so refuse rather
             * than hand a caller a digest it will believe. */
            const int32_t shared = attached(ep);
            if (shared != PAPERCLIP_EP_OK) {
                return shared;
            }
            image = ep->engine_aux;
            break;
        }
        case PAPERCLIP_EP_PLANE_BACK:
            image = &ep->back;
            break;
        case PAPERCLIP_EP_PLANE_AUX:
            image = &ep->aux;
            break;
        default:
            return fail(PAPERCLIP_EP_INVALID_ARGUMENT, "paperclip_ep_readback: no such plane");
        }
        /* constBits(), never bits(): the non-const accessor detaches the image
         * from whatever the engine is sharing with it, which would make the
         * measurement change the thing being measured — and, worse, could
         * leave the caller drawing into a buffer the engine no longer reads. */
        const uint32_t *pixels = reinterpret_cast<const uint32_t *>(image->constBits());
        if (pixels == nullptr) {
            return fail(PAPERCLIP_EP_NOT_OPEN, "paperclip_ep_readback: the plane has no pixels");
        }
        const int64_t available =
            static_cast<int64_t>(image->bytesPerLine() / 4) * image->height();
        const int64_t wanted = static_cast<int64_t>(len);
        if (wanted > available) {
            return fail(PAPERCLIP_EP_INVALID_ARGUMENT,
                        "paperclip_ep_readback: destination larger than the plane");
        }
        memcpy(out, pixels, static_cast<size_t>(wanted) * sizeof(uint32_t));
        return PAPERCLIP_EP_OK;
    } catch (const std::exception &error) {
        return fail(PAPERCLIP_EP_EXCEPTION, error.what());
    } catch (...) {
        return fail(PAPERCLIP_EP_EXCEPTION, "paperclip_ep_readback: unknown C++ exception");
    }
}

int32_t paperclip_ep_swap(paperclip_ep *ep, int32_t x, int32_t y, int32_t width,
                          int32_t height, int32_t content, int32_t mode, int32_t full)
{
    const int32_t status = check(ep);
    if (status != PAPERCLIP_EP_OK) {
        return status;
    }
    if (width <= 0 || height <= 0 || x < 0 || y < 0 || x + width > ep->front.width()
        || y + height > ep->front.height()) {
        return fail(PAPERCLIP_EP_INVALID_ARGUMENT, "paperclip_ep_swap: rectangle off the panel");
    }
    return present(ep, QRect(x, y, width, height), content, mode, full);
}

int32_t paperclip_ep_ghost_control(paperclip_ep *ep, int32_t mode)
{
    const int32_t status = check(ep);
    if (status != PAPERCLIP_EP_OK) {
        return status;
    }
    try {
        ep->engine->ghostControl(static_cast<EPFramebuffer::GhostControlMode>(mode));
        return PAPERCLIP_EP_OK;
    } catch (const std::exception &error) {
        return fail(PAPERCLIP_EP_EXCEPTION, error.what());
    } catch (...) {
        return fail(PAPERCLIP_EP_EXCEPTION, "ghostControl: unknown C++ exception");
    }
}

int32_t paperclip_ep_clear(paperclip_ep *ep)
{
    const int32_t status = check(ep);
    if (status != PAPERCLIP_EP_OK) {
        return status;
    }
    /* std::fill_n through the cached pointer, not `QImage::fill`, which is
     * non-const and detaches exactly as `bits()` does — the second instance of
     * the same WWW-29 bug, and the one that would have left a white clear
     * invisible on the glass while the handle was handed back. */
    const int32_t shared = attached(ep);
    if (shared != PAPERCLIP_EP_OK) {
        return shared;
    }
    std::fill_n(ep->pixels, ep->pixel_count, 0xFFFFFFFFu);
    /* The one place a full refresh is wanted: this runs before handing the
     * display back, and a partial update would leave the residue it exists to
     * remove. Mono mode 3 is the settled waveform. */
    return present(ep, QRect(0, 0, ep->front.width(), ep->front.height()),
                   PAPERCLIP_EP_CONTENT_MONO, 3, 1);
}

} // extern "C"
