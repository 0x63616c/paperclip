/*
 * The whole of Paperclip's C++ surface: nine functions, no Qt types, no
 * exceptions, no allocation the caller owns.
 *
 * §9 permits C++/Qt only where it is necessary, isolated under
 * platform/device/native and behind a small C ABI. This header is that ABI.
 * Everything above it is Rust; everything below it is the vendor's.
 *
 * ## Contract
 *
 * - **Thread affinity.** `paperclip_ep_open` must be called from the process's
 *   main thread — `libepaper` asserts "EPFramebuffer is being created outside
 *   the application's main thread!" (WWW-1). Every later call must come from
 *   that same thread, and returns PAPERCLIP_EP_WRONG_THREAD if it does not.
 * - **Ownership.** The handle is owned by the caller and freed only by
 *   `paperclip_ep_close`. The pixel buffer is owned by the *bridge* and is
 *   valid only until `paperclip_ep_close`; the caller never frees it.
 * - **Exceptions.** None escape. Every function is `noexcept` and catches
 *   everything, including `...`, mapping it to PAPERCLIP_EP_EXCEPTION.
 * - **Errors.** Every fallible function returns one of the status codes
 *   below. `paperclip_ep_last_error` returns a thread-local, NUL-terminated
 *   description of the most recent failure on the calling thread, valid until
 *   the next failing call on that thread. It is never NULL.
 * - **Shutdown order.** Release the display before closing: clear the panel,
 *   then `paperclip_ep_close`, then let the host restart xochitl. Closing
 *   without clearing leaves waveform residue on the glass that stock's own
 *   repaint does not remove (WWW-20).
 *
 * ## The no-detach invariant
 *
 * `EPFramebuffer::setBuffers` is an engine-internal init call, made once from
 * the backend's own constructor with images the engine allocated for itself
 * — calling it again from outside does nothing the engine ever reads back
 * (WWW-32, established on hardware). So the bridge never calls it. Instead it
 * finds the engine's own drawing surface — a `Format_RGB32` `QImage` already
 * living inside the `EPFramebuffer` object, identified by format and stride,
 * never by a fixed offset — and borrows it directly.
 *
 * The bridge does not hold a second `QImage` sharing that data — it holds a
 * raw pointer straight at the engine's own `QImage` object and reads its
 * `constBits()` directly, every time, rather than caching the address at the
 * `QImage` level. `constBits()` never disturbs the object it is called on,
 * but a non-const accessor — called on that object by anything, from either
 * side of this boundary — copies its pixel data onto fresh memory and leaves
 * the object pointing there instead. Writing through the pixel address cached
 * at `open` is how a frame reaches the panel; a detach from either side
 * severs it.
 *
 * So the bridge caches the engine surface's pixel address in `open`, hands
 * that cached pointer out, and never calls a non-const `QImage` accessor on
 * it. Every present re-checks the two agree and returns PAPERCLIP_EP_DETACHED
 * rather than presenting a frame the engine has never seen. WWW-29 found the
 * detach bug this guards against; WWW-32 found that the buffer being guarded
 * was the wrong one and moved the guard to the engine's own surface.
 */

#ifndef PAPERCLIP_EP_H
#define PAPERCLIP_EP_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Keep in step with `VendorStatus` in platform/device/src/error.rs. */
#define PAPERCLIP_EP_OK 0
#define PAPERCLIP_EP_INVALID_ARGUMENT 1
#define PAPERCLIP_EP_NOT_OPEN 2
#define PAPERCLIP_EP_LOCKED 3
#define PAPERCLIP_EP_EXCEPTION 4
#define PAPERCLIP_EP_WRONG_THREAD 5
#define PAPERCLIP_EP_OUT_OF_MEMORY 6
/* The drawing buffer is no longer the memory the engine holds. See the
 * no-detach invariant below; this is never a successful present. */
#define PAPERCLIP_EP_DETACHED 7

/* Bumped whenever this header changes shape. The Rust side checks it at open
 * so a stale .so and a new crate fail loudly instead of corrupting a call. */
#define PAPERCLIP_EP_ABI_VERSION 3u

/* Which of the three buffers `paperclip_ep_readback` copies out.
 *
 * FRONT is the engine's own drawing surface, found inside the already
 * constructed `EPFramebuffer` object rather than handed to it (WWW-32): the
 * caller's writes and the engine's scanout are the same memory, so FRONT is
 * both where the caller draws and what reaches the panel. BACK and AUX are
 * the bridge's own scratch buffers — never shared with the engine — kept for
 * bounds arithmetic and as inert readback planes.
 *
 * Reading FRONT back therefore answers "are the pixels I wrote still in the
 * memory the engine shares with me" — a different and weaker question than "is
 * that image on the glass", and the only one available from inside the
 * process. It answers even that much only while the no-detach invariant holds;
 * before WWW-30 it did not, and the readback was this side's buffer agreeing
 * with itself. */
#define PAPERCLIP_EP_PLANE_FRONT 0
#define PAPERCLIP_EP_PLANE_BACK 1
#define PAPERCLIP_EP_PLANE_AUX 2

/* Content types, mirroring `ContentType` in platform/device/src/waveform.rs. */
#define PAPERCLIP_EP_CONTENT_MONO 0
#define PAPERCLIP_EP_CONTENT_COLOR 1

typedef struct paperclip_ep paperclip_ep;

/* The ABI version this library was built with. */
uint32_t paperclip_ep_abi_version(void);

/*
 * Opens the vendor waveform engine and allocates the front, back and auxiliary
 * buffers. Main thread only. Fails with PAPERCLIP_EP_LOCKED when the vendor's
 * own `checkLockFile` reports another EPFramebuffer instance.
 */
int32_t paperclip_ep_open(paperclip_ep **out);

/* Closes the engine and frees its buffers. Safe to call with NULL. */
void paperclip_ep_close(paperclip_ep *ep);

/*
 * The panel geometry. `stride_pixels` may exceed `width`; it is the number of
 * uint32_t between the starts of consecutive rows.
 */
int32_t paperclip_ep_geometry(paperclip_ep *ep, int32_t *width, int32_t *height,
                              int32_t *stride_pixels);

/*
 * The drawing buffer: `height * stride_pixels` ARGB8888 pixels, little-endian,
 * so the bytes are B, G, R, 0xFF. Owned by the bridge. NULL on failure.
 *
 * The same address every time, cached at `open` before the engine was handed
 * the buffers, and valid until `paperclip_ep_close`. It is the memory the
 * engine shares — see the no-detach invariant above, which is why this is a
 * cached pointer and not a fresh `QImage::bits()` call.
 */
uint32_t *paperclip_ep_buffer(paperclip_ep *ep);

/*
 * Drives the rectangle to whatever is in the buffer.
 *
 * `content` is one of the PAPERCLIP_EP_CONTENT_* values, `mode` is the vendor's
 * waveform mode number, and `full` must be 0 for a partial update: a non-zero
 * `full` makes the backend refresh the whole panel regardless of the rectangle.
 */
int32_t paperclip_ep_swap(paperclip_ep *ep, int32_t x, int32_t y, int32_t width,
                          int32_t height, int32_t content, int32_t mode,
                          int32_t full);

/* Sets the engine's ghost-suppression mode. */
/* Copy one plane out, for comparison against what was sent.
 *
 * `out` receives `len` uint32 of ARGB8888 at the plane's own stride. Reads
 * through Qt's *const* accessor, so it cannot detach the image and cannot
 * perturb what the engine is sharing — a readback that changed the thing it
 * measured would be worse than none.
 *
 * With the no-detach invariant holding, a FRONT readback reads the memory the
 * engine reads. Without it the call is sound but vacuous: it compares this
 * side's private copy against itself and matches no matter what the engine
 * holds. That is exactly what WWW-23's "front came back byte-identical"
 * measured, and why the bug it missed stayed silent until WWW-29.
 *
 * Never a claim about the panel. See the note in the header comment. */
int32_t paperclip_ep_readback(paperclip_ep *ep, int32_t plane, uint32_t *out, int32_t len);

int32_t paperclip_ep_ghost_control(paperclip_ep *ep, int32_t mode);

/* Drives the whole panel white with a settled waveform.
 *
 * Fills through the cached pointer rather than `QImage::fill`, which is
 * non-const and would detach. */
int32_t paperclip_ep_clear(paperclip_ep *ep);

/* A description of the last failure on this thread. Never NULL. */
const char *paperclip_ep_last_error(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* PAPERCLIP_EP_H */
