/* SPDX-License-Identifier: GPL-2.0-only WITH Linux-syscall-note */
/*
 * Userspace interface of /dev/rosemary. `darwin::tls` mirrors these values;
 * change both together and bump ROSEMARY_ABI_VERSION.
 */
#ifndef _UAPI_LINUX_ROSEMARY_H
#define _UAPI_LINUX_ROSEMARY_H

#include <linux/ioctl.h>
#include <linux/types.h>

#define ROSEMARY_ABI_VERSION 1

#define ROSEMARY_IOC_MAGIC 0xD7

/* The module's ABI version. */
#define ROSEMARY_IOC_VERSION _IOR(ROSEMARY_IOC_MAGIC, 0, __u32)
/* ROSEMARY_INFO_* flags describing this kernel. */
#define ROSEMARY_IOC_INFO _IOR(ROSEMARY_IOC_MAGIC, 1, __u64)
/*
 * Set the calling thread's TPIDRRO_EL0, kept across context switches until the
 * thread exits or execs. Zero releases the thread.
 */
#define ROSEMARY_IOC_SET_TPIDRRO _IOW(ROSEMARY_IOC_MAGIC, 2, __u64)

/*
 * The kernel enters through the KPTI trampoline, which clears TPIDRRO_EL0 on
 * every exception from EL0, so the value does not survive a syscall.
 */
#define ROSEMARY_INFO_KPTI (1ULL << 0)

#endif
