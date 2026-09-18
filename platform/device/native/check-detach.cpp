// The bridge's own no-detach invariant, exercised end to end without a tablet.
//
// `check-sharing.cpp` pins the Qt behaviour. This pins *our* use of it: that
// `paperclip_ep_open` finds the engine's own drawing surface by format and
// stride rather than trusting a fixed offset, that `paperclip_ep_buffer` and
// `paperclip_ep_clear` go through the cached address rather than through a
// non-const QImage accessor, and that a present refuses with
// PAPERCLIP_EP_DETACHED instead of pushing a frame the engine cannot see.
//
// ## How it runs the real code path
//
// It includes the bridge's translation unit and supplies its own
// `EPFramebuffer`, so `paperclip_ep_open` runs exactly as shipped — preflight,
// `find_draw_surface`, caching — with no vendor library and no e-ink.
//
// `EPFramebuffer::instance()` does NOT return a normally-constructed object of
// the class `ep_abi.hpp` declares — that declaration has no data members, on
// purpose (WWW-20; the ABI check depends on nothing but type *names*), so it
// has nothing at `this+0x88` to find. Real production code never has a
// complete definition of the vendor's object either: `find_draw_surface`
// treats `EPFramebuffer *` as raw memory and reads `QImage`s out of it by
// offset. This stub does the same thing on purpose: it hands back a pointer
// into a raw byte buffer with two real `QImage`s placed inside at the offsets
// `paperclip_ep.cpp` scans — a `Format_RGB32` surface at `+0x88`, the real
// drawing surface, and a `Format_Grayscale8` decoy of identical dimensions at
// `+0xa8`, mirroring exactly what WWW-32 found on the tablet. A stub that put
// only the RGB32 image there would pass even if the format check were deleted
// and only the dimensions check remained; the decoy is what that regression
// needs to catch.
//
// It needs a disposable /usr/share/remarkable, because `preflight()` insists on
// one; `check-host.sh` makes it. Run it from there, not directly.

#include "paperclip_ep.cpp" // NOLINT — deliberate: see above.

#include <cstdint>
#include <cstdio>
#include <new>

namespace {

/* Raw storage for the fabricated engine object, sized past the last offset
 * `find_draw_surface` probes. `alignas(void*)` is enough: every candidate
 * offset (0x88, 0xa8, 0xc8) is 8-aligned, and QImage's one d-pointer member
 * needs no more than that. */
constexpr std::ptrdiff_t kFrontOffset = 0x88;
constexpr std::ptrdiff_t kDecoySlotOffset = 0xa8;
constexpr std::size_t kStorageSize = 0xd0;

struct alignas(void *) EngineStorage {
    unsigned char bytes[kStorageSize] = {};
};

EngineStorage g_engine_storage;
bool g_engine_ready = false;

QImage *front_slot()
{
    return reinterpret_cast<QImage *>(g_engine_storage.bytes + kFrontOffset);
}

QImage *decoy_slot()
{
    return reinterpret_cast<QImage *>(g_engine_storage.bytes + kDecoySlotOffset);
}

int swaps = 0;

constexpr int kProbe = 1000 * 1620 + 811;

int failures = 0;

void expect(bool condition, const char *what)
{
    std::printf("%-4s %s\n", condition ? "ok" : "FAIL", what);
    if (!condition) {
        ++failures;
    }
}

const uint32_t *engine_view()
{
    return reinterpret_cast<const uint32_t *>(front_slot()->constBits());
}

} // namespace

EPFramebuffer *EPFramebuffer::instance()
{
    if (!g_engine_ready) {
        /* Format_RGB32: the real drawing surface, at the offset WWW-32 found
         * on image 20260827113527. */
        new (front_slot()) QImage(1620, 2160, QImage::Format_RGB32);
        /* Format_Grayscale8, same dimensions: the engine-internal buffer that
         * published third-party notes mistook for the drawing surface. Same
         * size as the real one on purpose — dimensions alone cannot tell them
         * apart, which is the entire reason `find_draw_surface` also checks
         * format and stride. */
        new (decoy_slot()) QImage(1620, 2160, QImage::Format_Grayscale8);
        g_engine_ready = true;
    }
    return reinterpret_cast<EPFramebuffer *>(g_engine_storage.bytes);
}

void EPFramebuffer::swapBuffers(QRect, EPScreenMode, QFlags<UpdateFlag>)
{
    ++swaps;
}

void EPFramebuffer::ghostControl(GhostControlMode) {}

bool EPFramebuffer::checkLockFile()
{
    return true;
}

int main()
{
    paperclip_ep *ep = nullptr;
    const int32_t opened = paperclip_ep_open(&ep);
    if (opened != PAPERCLIP_EP_OK || ep == nullptr) {
        std::printf("FAIL open rc=%d err=[%s]\n", opened, paperclip_ep_last_error());
        std::printf("     (a missing /usr/share/remarkable is the usual cause; "
                    "run this through check-host.sh)\n");
        return 1;
    }

    expect(ep->engine_aux == front_slot(),
           "the bridge found the RGB32 surface, not the Grayscale8 decoy at +0xa8");

    uint32_t *buffer = paperclip_ep_buffer(ep);
    expect(buffer != nullptr, "the bridge hands out a buffer");
    expect(engine_view() == buffer,
           "the buffer handed out IS the memory the engine holds");

    /* The regression assertion. With `paperclip_ep_buffer` spelled
     * `front.bits()` on a shared image, this write would land in memory the
     * engine has never heard of. */
    buffer[kProbe] = 0xFF112233u;
    expect(engine_view()[kProbe] == 0xFF112233u,
           "a frame drawn into that buffer is visible to the engine");

    expect(paperclip_ep_buffer(ep) == buffer,
           "the address is stable across calls, so no call can orphan a frame");

    expect(paperclip_ep_swap(ep, 0, 0, 1620, 2160, PAPERCLIP_EP_CONTENT_MONO, 3, 1)
               == PAPERCLIP_EP_OK,
           "an attached present succeeds");
    expect(swaps == 1, "and reached the engine");

    /* Readback must be the engine's memory, not a private copy that agrees
     * with itself — the flaw in WWW-23's evidence. */
    uint32_t probe[4] = {0, 0, 0, 0};
    expect(paperclip_ep_readback(ep, PAPERCLIP_EP_PLANE_FRONT, probe, 4) == PAPERCLIP_EP_OK,
           "the front plane reads back while attached");

    expect(paperclip_ep_clear(ep) == PAPERCLIP_EP_OK, "clear succeeds");
    expect(engine_view()[kProbe] == 0xFFFFFFFFu,
           "and the white it wrote is visible to the engine, not to a detached copy");
    expect(swaps == 2, "clear presents as well as fills");

    /* Now force the failure this invariant exists for. Nothing in the bridge
     * calls a non-const accessor on the engine's surface — this reaches in to
     * prove the guard fires if something ever did. `bits()` on an unshared
     * image does not detach, because there is nothing to detach *from*; a
     * second reference is needed first, standing in for whatever inside the
     * engine might also hold this QImageData. */
    QImage shared_elsewhere = *front_slot();
    (void)front_slot()->bits();
    expect(front_slot()->constBits() != reinterpret_cast<const uchar *>(ep->pixels),
           "the engine's surface has been forced off the memory the bridge cached");

    expect(paperclip_ep_swap(ep, 0, 0, 1620, 2160, PAPERCLIP_EP_CONTENT_MONO, 3, 1)
               == PAPERCLIP_EP_DETACHED,
           "a detached present is refused, not reported as success");
    expect(swaps == 2, "and never reached the engine");
    expect(paperclip_ep_clear(ep) == PAPERCLIP_EP_DETACHED, "a detached clear is refused too");
    expect(paperclip_ep_readback(ep, PAPERCLIP_EP_PLANE_FRONT, probe, 4)
               == PAPERCLIP_EP_DETACHED,
           "and a detached front readback refuses rather than flattering itself");

    paperclip_ep_close(ep);

    std::printf("%s (%d failure%s)\n", failures == 0 ? "PASS" : "FAILED", failures,
                failures == 1 ? "" : "s");
    return failures == 0 ? 0 : 1;
}
