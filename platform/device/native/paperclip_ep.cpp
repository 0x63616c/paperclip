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

#include <QtCore/QCoreApplication>
#include <QtCore/QRect>
#include <QtGui/QImage>

#include <cstring>
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

/* The engine's SWTCON backend loads these at construction and calls abort() if
 * any is missing or the wrong size — observed, not inferred: a container run
 * with none of them present printed
 *
 *     /usr/share/remarkable/ct33_std.bin: file does not exist.
 *     Failed to initialize SWTCON.
 *
 * and raised SIGABRT. With a wrong-sized file it printed "unexpected size".
 * `ct33` is the panel lot; the library also searches by lot and TFT, so a
 * different panel may want different names. Hence: check for *existence and
 * non-emptiness*, name the file that is wrong, and let the engine do the real
 * validation. Guessing the correct size here would trade one abort for a
 * wrong refusal. */
const char *const kWaveformFiles[] = {
    "/usr/share/remarkable/ct33_std.bin",
    "/usr/share/remarkable/ct33_best.bin",
    "/usr/share/remarkable/ct33_fast.bin",
    "/usr/share/remarkable/ct33_pen.bin",
};

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
    for (const char *path : kWaveformFiles) {
        struct stat info = {};
        if (stat(path, &info) != 0) {
            try {
                const std::string message = std::string("waveform table missing: ") + path
                                            + " (the engine aborts rather than failing)";
                return fail(PAPERCLIP_EP_NOT_OPEN, message.c_str());
            } catch (...) {
                return fail(PAPERCLIP_EP_NOT_OPEN, "a waveform table is missing");
            }
        }
        if (info.st_size == 0) {
            return fail(PAPERCLIP_EP_NOT_OPEN, "a waveform table is empty");
        }
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

        /* UNVERIFIED: which element of the tuple the engine presents from, and
         * whether `aux` is the shadow or the target. If the first device build
         * shows drawing land on the wrong page, this line is where to swap
         * them, and ADR-0009 is where to record which way round it was. */
        engine->setBuffers(std::make_tuple(handle->front, handle->back), &handle->aux);

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
