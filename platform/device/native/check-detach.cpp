// The bridge's own no-detach invariant, exercised end to end without a tablet.
//
// `check-sharing.cpp` pins the Qt behaviour. This pins *our* use of it: that
// `paperclip_ep_open` caches the pixel address at the one moment it is safe to,
// that `paperclip_ep_buffer` and `paperclip_ep_clear` go through that address
// rather than through a non-const QImage accessor, and that a present refuses
// with PAPERCLIP_EP_DETACHED instead of pushing a frame the engine cannot see.
//
// ## How it runs the real code path
//
// It includes the bridge's translation unit and supplies its own
// `EPFramebuffer`, so `paperclip_ep_open` runs exactly as shipped — preflight,
// allocation, caching, `setBuffers` — with no vendor library and no e-ink. The
// stub's `setBuffers` is not a no-op: it stores the tuple elements by
// `QImage::operator=`, which is what the real one does (WWW-29, 0x328c0). That
// is the whole point. A stub that merely ignored its arguments would leave the
// refcount at 1, nothing would ever detach, and this test would pass over the
// bug it exists to catch.
//
// It needs a disposable /usr/share/remarkable, because `preflight()` insists on
// one; `check-host.sh` makes it. Run it from there, not directly.

#include "paperclip_ep.cpp" // NOLINT — deliberate: see above.

#include <cstdint>
#include <cstdio>

namespace {

/* The three members the disassembly shows `setBuffers` writing: `this+0x88`
 * from std::get<0>, `this+0xa0` from the aux argument, `this+0xa8` from
 * std::get<1>. Held at file scope because the declaration in `ep_abi.hpp` has
 * no data members and gains none here. */
QImage held_front;
QImage held_aux;
QImage held_back;
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
    return reinterpret_cast<const uint32_t *>(held_front.constBits());
}

} // namespace

EPFramebuffer *EPFramebuffer::instance()
{
    static EPFramebuffer only;
    return &only;
}

void EPFramebuffer::setBuffers(std::tuple<QImage, QImage> buffers, QImage *aux)
{
    held_front = std::get<0>(buffers);
    held_aux = aux != nullptr ? *aux : held_front;
    held_back = std::get<1>(buffers);
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

    expect(!held_front.isNull(), "the engine was handed a front buffer");

    uint32_t *buffer = paperclip_ep_buffer(ep);
    expect(buffer != nullptr, "the bridge hands out a buffer");
    expect(engine_view() == buffer,
           "the buffer handed out IS the memory the engine holds");

    /* The regression assertion. With `paperclip_ep_buffer` spelled
     * `front.bits()`, the call above detached and this write lands in memory
     * the engine has never heard of. */
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
     * does this any more; the test reaches in to prove the guard fires. */
    (void)ep->front.bits();
    expect(ep->front.constBits() != reinterpret_cast<const uchar *>(ep->pixels),
           "the front image has been forced off the engine's memory");

    expect(paperclip_ep_swap(ep, 0, 0, 1620, 2160, PAPERCLIP_EP_CONTENT_MONO, 3, 1)
               == PAPERCLIP_EP_DETACHED,
           "a detached present is refused, not reported as success");
    expect(swaps == 2, "and never reached the engine");
    expect(paperclip_ep_clear(ep) == PAPERCLIP_EP_DETACHED, "a detached clear is refused too");
    expect(paperclip_ep_readback(ep, PAPERCLIP_EP_PLANE_FRONT, probe, 4)
               == PAPERCLIP_EP_DETACHED,
           "and a detached front readback refuses rather than flattering itself");

    held_front = QImage();
    held_back = QImage();
    held_aux = QImage();
    paperclip_ep_close(ep);

    std::printf("%s (%d failure%s)\n", failures == 0 ? "PASS" : "FAILED", failures,
                failures == 1 ? "" : "s");
    return failures == 0 ? 0 : 1;
}
