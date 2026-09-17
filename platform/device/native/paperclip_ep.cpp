/*
 * The Paperclip side of the vendor waveform engine.
 *
 * ## Status
 *
 * **This file has never been compiled.** It needs Qt 6.10.3 headers from the
 * reMarkable SDK and `libqsgepaper.so` copied off the tablet, and WWW-3's
 * first pass had neither — the device was unreachable. What *is* verified is
 * the part that decides whether it links: `check-abi.sh` proves the
 * declarations in `ep_abi.hpp` generate exactly the symbols WWW-20 read off
 * the library. Treat everything below as unvalidated until a build says
 * otherwise, and see ADR-0009 for what remains open.
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

#include <QtGui/QImage>
#include <QtCore/QRect>

#include <cstring>
#include <exception>
#include <new>
#include <pthread.h>
#include <string>
#include <tuple>

#include "ep_abi.hpp"

namespace {

/* Panel geometry. 1620 x 2160 ARGB8888, confirmed from the device tree
 * (WWW-1) and corroborated by rmweb's device profile (ADR-0007). */
constexpr int kPanelWidth = 1620;
constexpr int kPanelHeight = 2160;

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

} // namespace

struct paperclip_ep {
    EPFramebuffer *engine = nullptr;
    /* The bridge owns these for as long as the handle lives. EPFramebuffer is
     * handed the two page buffers and the auxiliary one by setBuffers; Rust
     * borrows the bits of `front` and never frees anything. */
    QImage front;
    QImage back;
    QImage aux;
    pthread_t owner = {};
};

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

    try {
        EPFramebuffer *engine = EPFramebuffer::instance();
        if (engine == nullptr) {
            return fail(PAPERCLIP_EP_NOT_OPEN, "EPFramebuffer::instance() returned null");
        }
        /* The vendor's own side of the advisory registry Xochitl participates
         * in. Holding DRM master is not display ownership on this device. */
        if (!engine->checkLockFile()) {
            return fail(PAPERCLIP_EP_LOCKED,
                        "EPFramebuffer::checkLockFile() reports another instance");
        }

        paperclip_ep *handle = new (std::nothrow) paperclip_ep();
        if (handle == nullptr) {
            return fail(PAPERCLIP_EP_OUT_OF_MEMORY, "allocating the bridge handle failed");
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

        /* UNVERIFIED: which element of the tuple the engine presents from, and
         * whether `aux` is the shadow or the target. If the first device build
         * shows drawing land on the wrong page, this line is where to swap
         * them, and ADR-0009 is where to record which way round it was. */
        engine->setBuffers(std::make_tuple(handle->front, handle->back), &handle->aux);

        *out = handle;
        g_last_error.clear();
        return PAPERCLIP_EP_OK;
    } catch (const std::exception &error) {
        return fail(PAPERCLIP_EP_EXCEPTION, error.what());
    } catch (...) {
        return fail(PAPERCLIP_EP_EXCEPTION, "paperclip_ep_open: unknown C++ exception");
    }
}

void paperclip_ep_close(paperclip_ep *ep)
{
    if (ep == nullptr) {
        return;
    }
    try {
        delete ep;
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

/* The vendor's EPScreenMode and UpdateFlag values are opaque to us: they are
 * enums in a closed header we do not have. The engine's mode numbers are
 * passed through as integers and reinterpreted here, which is the one place
 * the guess lives. */
int32_t present(paperclip_ep *ep, QRect rect, int32_t content, int32_t mode,
                int32_t full) noexcept
{
    try {
        const int screen_mode = (content == PAPERCLIP_EP_CONTENT_COLOR ? 0x100 : 0) | mode;
        const int flags = full != 0 ? 1 : 0;
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
        *width = ep->front.width();
        *height = ep->front.height();
        *stride_pixels = static_cast<int32_t>(ep->front.bytesPerLine() / 4);
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
    try {
        return reinterpret_cast<uint32_t *>(ep->front.bits());
    } catch (...) {
        fail(PAPERCLIP_EP_EXCEPTION, "paperclip_ep_buffer: unknown C++ exception");
        return nullptr;
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
    try {
        ep->front.fill(0xFFFFFFFFu);
    } catch (...) {
        return fail(PAPERCLIP_EP_EXCEPTION, "paperclip_ep_clear: unknown C++ exception");
    }
    /* The one place a full refresh is wanted: this runs before handing the
     * display back, and a partial update would leave the residue it exists to
     * remove. Mono mode 3 is the settled waveform. */
    return present(ep, QRect(0, 0, ep->front.width(), ep->front.height()),
                   PAPERCLIP_EP_CONTENT_MONO, 3, 1);
}

} // extern "C"
