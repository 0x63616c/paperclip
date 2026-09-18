// The Qt behaviour the whole display path rests on, reproduced in isolation.
//
// A `QImage`'s pixel data is refcounted: `constBits()` never disturbs it, but
// a non-const accessor — `bits()`, `fill()`, and the like — copies the data
// onto fresh memory whenever the refcount says someone else might be looking
// at it, and hands back a pointer to the copy instead. Called on an image
// nothing else references, the same accessor is a no-op: there is nothing to
// copy away from. `paperclip_ep.cpp` depends on the first case never
// happening to the engine's own drawing surface once the bridge has cached
// its address, which is why it never calls a non-const accessor on it.
//
// (Earlier revisions of this file framed this around `EPFramebuffer::setBuffers`
// specifically, on the strength of WWW-29's disassembly of it: the bridge's
// own `front` and the engine's copy of it, connected by that call. WWW-32
// found, on hardware, that `setBuffers` never connected them at all — see
// ADR-0009. The Qt mechanism below is unaffected; only which two objects it
// was ever protecting changed.)
//
// This file needs Qt and nothing else: no `libqsgepaper.so`, no reMarkable SDK,
// no tablet. It asserts both halves:
//
//   1. a pointer cached while the image is unshared stays valid, and writes
//      through it are visible to the shared copy — the fix;
//   2. calling `bits()` on the original after sharing breaks exactly that —
//      the bug, which every present between WWW-3 and WWW-30 hit.
//
// If (2) ever stops failing, Qt changed its sharing rules and the invariant in
// `paperclip_ep.cpp` needs rereading rather than deleting.

#include <QtGui/QImage>

#include <cstdint>
#include <cstdio>

namespace {

constexpr int kWidth = 1620;
constexpr int kHeight = 2160;

/* Somewhere past the first row, so a stride mistake cannot make this pass. */
constexpr int kProbe = 1000 * 1620 + 811;

int failures = 0;

void expect(bool condition, const char *what)
{
    std::printf("%-4s %s\n", condition ? "ok" : "FAIL", what);
    if (!condition) {
        ++failures;
    }
}

const uint32_t *view(const QImage &image)
{
    return reinterpret_cast<const uint32_t *>(image.constBits());
}

} // namespace

int main()
{
    QImage front(kWidth, kHeight, QImage::Format_ARGB32);
    if (front.isNull()) {
        std::printf("FAIL could not allocate a %dx%d image\n", kWidth, kHeight);
        return 1;
    }
    front.fill(0xFFFFFFFFu);

    /* What `paperclip_ep_open` does, in the one moment the image is unshared:
     * take the address while the refcount is 1, so nothing can be copied. */
    uint32_t *cached = reinterpret_cast<uint32_t *>(front.bits());
    expect(cached != nullptr, "the unshared image has pixels");

    /* What `setBuffers` does: a refcounted shallow assignment. From here the
     * engine and the bridge are looking at one allocation. */
    QImage engine_holds = front;

    expect(view(engine_holds) == reinterpret_cast<const uint32_t *>(cached),
           "after the shallow copy, the engine's image shares the cached memory");

    /* (1) The fix: draw through the cached pointer, and the engine sees it. */
    cached[kProbe] = 0xFF112233u;
    expect(view(engine_holds)[kProbe] == 0xFF112233u,
           "a write through the cached pointer is visible to the engine's copy");

    /* (2) The bug: one non-const accessor, and the two part company. Qt
     * reports nothing — the call returns a perfectly good pointer to the wrong
     * memory. */
    uint32_t *detached = reinterpret_cast<uint32_t *>(front.bits());
    expect(detached != cached,
           "bits() on a shared image detaches it onto fresh memory");
    expect(view(front) != reinterpret_cast<const uint32_t *>(cached),
           "the detached original no longer shares the engine's memory");

    detached[kProbe] = 0xFF445566u;
    expect(view(engine_holds)[kProbe] == 0xFF112233u,
           "after the detach the engine's copy no longer sees our writes");
    expect(view(front)[kProbe] == 0xFF445566u,
           "and the orphaned buffer happily accepts them, which is why this was silent");

    /* The second instance of the same bug, for the same reason: QImage::fill is
     * non-const, so `paperclip_ep_clear` written the obvious way detaches too. */
    QImage other(kWidth, kHeight, QImage::Format_ARGB32);
    other.fill(0xFFFFFFFFu);
    const uint32_t *before_fill = view(other);
    QImage shared_with_engine = other;
    other.fill(0xFF000000u);
    expect(view(other) != before_fill, "QImage::fill on a shared image detaches as well");
    expect(view(shared_with_engine)[kProbe] == 0xFFFFFFFFu,
           "so a clear written as fill() never reaches the engine");

    /* constBits() is the accessor that does not. */
    QImage held = shared_with_engine;
    const uint32_t *const_view = view(shared_with_engine);
    expect(view(held) == const_view, "constBits() does not detach");

    std::printf("%s (%d failure%s)\n", failures == 0 ? "PASS" : "FAILED", failures,
                failures == 1 ? "" : "s");
    return failures == 0 ? 0 : 1;
}
