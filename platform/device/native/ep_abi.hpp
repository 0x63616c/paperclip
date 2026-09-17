// Declarations of the vendor's EPFramebuffer, and nothing else.
//
// This file exists to be *checked*. `libqsgepaper.so` is proprietary
// (`LICENSE: CLOSED`), ships no header, and is never vendored into this
// repository — so the only way to call it is to redeclare its C++ interface
// and rely on Itanium name mangling to match. A redeclaration that is subtly
// wrong does not fail to build; it fails to link, or worse, links to the wrong
// overload.
//
// So the declarations are kept apart from any use of them, depend on nothing
// but type *names*, and are verified two ways:
//
//   1. `check-abi.sh` compiles this header with stub Qt declarations on any
//      host, dumps the symbols it generates, demangles them, and compares them
//      against `vendor-abi.txt` — the signatures WWW-20 read off the device.
//      This runs on a Mac, with no SDK and no vendor library.
//   2. The same script, given a copy of `libqsgepaper.so`, additionally checks
//      that every generated mangled symbol is actually exported by it.
//
// Signatures are from WWW-20's inspection of `libqsgepaper.so` on image
// `20260827113527` (see ADR-0007). Return types are *not* part of Itanium
// mangling for ordinary functions, so the ones below are this project's
// reading and are the part of the file most likely to be wrong; the argument
// lists are the part the linker checks.
//
// Interface shape borrowed from MaximeRivest/quill (MIT), which arrived at the
// same wrapper independently. No Quill code is copied. See ADR-0009.

#ifndef PAPERCLIP_EP_ABI_HPP
#define PAPERCLIP_EP_ABI_HPP

// Qt and std types appear by name only. The implementation includes the real
// headers before this one; the ABI check declares stubs. Either way the
// mangling is the same, which is the entire point of the split.
class QImage;
class QRect;
class QRegion;
template <typename Enum> class QFlags;

class EPScreenModeMap;
enum EPScreenMode : int;

// `EPFramebuffer` is a `QObject` on this image — it exports `qt_metacall`,
// `qt_metacast`, `staticMetaObject` and a `framebufferUpdated(const QRect&)`
// signal. That is not declared here because none of it is called and the base
// class does not affect the mangling of the members that are. It is recorded
// because it raises a real question: a QObject may expect a running
// `QCoreApplication`. See ADR-0009, "what would make this wrong".
class EPFramebuffer {
public:
    // Both nested, not global. `UpdateFlag` being nested is what the first run
    // of `check-abi.sh` against the real library caught: the two `swapBuffers`
    // overloads mangle `QFlags<EPFramebuffer::UpdateFlag>` as `NS_10UpdateFlagE`
    // and the global reading produced `10UpdateFlagE`, which links to nothing.
    enum GhostControlMode : int;
    enum UpdateFlag : int;

    static EPFramebuffer *instance();

    void setBuffers(std::tuple<QImage, QImage> buffers, QImage *aux);

    void swapBuffers(QRect rect, EPScreenMode mode, QFlags<UpdateFlag> flags);
    void swapBuffers(const QRegion &region, const EPScreenModeMap &modes,
                     QFlags<UpdateFlag> flags);

    void ghostControl(GhostControlMode mode);

    bool checkLockFile();
    void handleCrash();
};

#endif // PAPERCLIP_EP_ABI_HPP
