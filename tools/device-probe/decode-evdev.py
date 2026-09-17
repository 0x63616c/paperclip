#!/usr/bin/env python3
"""Decode a raw evdev capture from the Paper Pro into readable events.

    cat /dev/input/event3 > touch.bin      # on the tablet
    ./decode-evdev.py touch.bin            # on the Mac

Used to turn calibration tap captures into (panel target, raw value) pairs.
Layout is the 64-bit aarch64 `struct input_event`: two 64-bit timeval fields
then u16 type, u16 code, s32 value -- 24 bytes per event.
"""
import struct
import sys

EVENT_SIZE = 24

TYPES = {0: "SYN", 1: "KEY", 3: "ABS"}

# Pen (event2) and touch (event3) axes present on this device, per WWW-1.
ABS_CODES = {
    0x00: "X", 0x01: "Y", 0x18: "PRESSURE", 0x19: "DISTANCE",
    0x1A: "TILT_X", 0x1B: "TILT_Y",
    0x2F: "MT_SLOT", 0x30: "MT_TOUCH_MAJOR", 0x35: "MT_POSITION_X",
    0x36: "MT_POSITION_Y", 0x37: "MT_TOOL_TYPE", 0x39: "MT_TRACKING_ID",
    0x3A: "MT_PRESSURE", 0x3B: "MT_DISTANCE",
}
KEY_CODES = {
    0x14A: "BTN_TOUCH", 0x140: "BTN_TOOL_PEN", 0x141: "BTN_TOOL_RUBBER",
    0x14B: "BTN_STYLUS", 0x14C: "BTN_STYLUS2",
}


def decode(path):
    """Yield (t_relative, type_name, code_name, value) for each event."""
    with open(path, "rb") as fh:
        blob = fh.read()
    base = None
    for offset in range(0, len(blob) - EVENT_SIZE + 1, EVENT_SIZE):
        sec, usec, etype, code, value = struct.unpack_from("<qqHHi", blob, offset)
        stamp = sec + usec / 1e6
        if base is None:
            base = stamp
        if etype == 3:
            name = ABS_CODES.get(code, hex(code))
        elif etype == 1:
            name = KEY_CODES.get(code, hex(code))
        else:
            name = str(code)
        yield stamp - base, TYPES.get(etype, str(etype)), name, value


def main(argv):
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 64

    # Multitouch protocol B delimits each contact with MT_TRACKING_ID, NOT with
    # BTN_TOUCH: BTN_TOUCH only goes 1 on the first finger down and 0 on the
    # last finger up, so keying on it merges a whole sequence of taps into one
    # contact and loses every tap in between.
    contacts = []          # one (x, y) per completed touch or pen contact
    pending = {}
    for stamp, etype, name, value in decode(argv[1]):
        if etype == 0:
            continue
        print(f"t+{stamp:8.3f}  {etype:4}  {name:16} {value}")
        if name == "MT_TRACKING_ID":
            if value == -1:
                if "x" in pending and "y" in pending:
                    contacts.append((pending["x"], pending["y"]))
                pending = {}
            else:
                pending = {}
        elif name in ("MT_POSITION_X", "X"):
            pending["x"] = value
        elif name in ("MT_POSITION_Y", "Y"):
            pending["y"] = value
        elif name == "BTN_TOUCH" and value == 0 and "MT" not in str(pending):
            # Pen path: the tip has no tracking id, so its lift delimits it.
            if "x" in pending and "y" in pending:
                contacts.append((pending["x"], pending["y"]))
            pending = {}

    print(f"\n{len(contacts)} completed contacts:")
    for index, (x, y) in enumerate(contacts, 1):
        print(f"  {index:2}. x={x:6} y={y:6}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
