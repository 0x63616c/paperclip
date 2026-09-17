/*
 * evname -- identify input devices by EVIOCGNAME, never by event-node order.
 *
 * The Paper Pro's node ordering differs from earlier reMarkables, so binding to
 * /dev/input/event2 or event3 by number is a latent bug. rmweb's device profile
 * is explicit about this: "Resolve by NAME via EVIOCGNAME, never by eventN."
 * This prints the mapping and classifies each node from its advertised axes so
 * platform/device can look devices up rather than assume them.
 */
#define _GNU_SOURCE
#include <dirent.h>
#include <fcntl.h>
#include <linux/input.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#define BITS_PER_LONG (8 * (int)sizeof(long))
#define NBITS(x) (((x) - 1) / BITS_PER_LONG + 1)
#define TEST_BIT(bit, array) \
    (((array)[(bit) / BITS_PER_LONG] >> ((bit) % BITS_PER_LONG)) & 1)

static void describe_axis(int fd, int axis, const char *label)
{
    struct input_absinfo info;

    if (ioctl(fd, EVIOCGABS(axis), &info) < 0)
        return;
    printf("      %-18s min=%-7d max=%-7d res=%d\n", label, info.minimum,
           info.maximum, info.resolution);
}

int main(void)
{
    struct dirent *entry;
    DIR *dir = opendir("/dev/input");

    if (!dir) {
        perror("opendir /dev/input");
        return 1;
    }

    while ((entry = readdir(dir))) {
        char path[280];
        char name[256] = "";
        unsigned long keys[NBITS(KEY_MAX)];
        unsigned long absbits[NBITS(ABS_MAX)];
        const char *role;
        int fd;

        if (strncmp(entry->d_name, "event", 5) != 0)
            continue;
        snprintf(path, sizeof path, "/dev/input/%s", entry->d_name);
        fd = open(path, O_RDONLY | O_CLOEXEC);
        if (fd < 0)
            continue;

        if (ioctl(fd, EVIOCGNAME(sizeof name), name) < 0)
            strcpy(name, "<unnamed>");
        memset(keys, 0, sizeof keys);
        memset(absbits, 0, sizeof absbits);
        ioctl(fd, EVIOCGBIT(EV_KEY, sizeof keys), keys);
        ioctl(fd, EVIOCGBIT(EV_ABS, sizeof absbits), absbits);

        /* Classify from advertised capability, not from the node number. */
        if (TEST_BIT(BTN_TOOL_PEN, keys))
            role = "pen";
        else if (TEST_BIT(ABS_MT_POSITION_X, absbits))
            role = "touch";
        else if (TEST_BIT(KEY_POWER, keys))
            role = "power-key";
        else
            role = "other";

        printf("%-20s %-28s role=%s\n", path, name, role);
        if (!strcmp(role, "pen")) {
            describe_axis(fd, ABS_X, "ABS_X");
            describe_axis(fd, ABS_Y, "ABS_Y");
            describe_axis(fd, ABS_PRESSURE, "ABS_PRESSURE");
            printf("      eraser=%s\n",
                   TEST_BIT(BTN_TOOL_RUBBER, keys) ? "yes" : "no");
        } else if (!strcmp(role, "touch")) {
            describe_axis(fd, ABS_MT_POSITION_X, "ABS_MT_POSITION_X");
            describe_axis(fd, ABS_MT_POSITION_Y, "ABS_MT_POSITION_Y");
            describe_axis(fd, ABS_MT_SLOT, "ABS_MT_SLOT");
        }
        close(fd);
    }
    closedir(dir);
    return 0;
}
