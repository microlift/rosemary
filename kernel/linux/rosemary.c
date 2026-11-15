// SPDX-License-Identifier: GPL-2.0-only
/*
 * Gives a thread a TPIDRRO_EL0 that persists, as XNU does for Darwin's TSD
 * base. Linux zeroes the register when it switches to a native task; a preempt
 * notifier writes the thread's value back after that, before EL0 runs again.
 */

#include <linux/fs.h>
#include <linux/miscdevice.h>
#include <linux/module.h>
#include <linux/preempt.h>
#include <linux/sched.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/tracepoint.h>
#include <linux/uaccess.h>
#include <asm/mmu.h>
#include <asm/sysreg.h>

#include "rosemary.h"

#ifndef CONFIG_ARM64
#error "TPIDRRO_EL0 is an arm64 register"
#endif
#ifndef CONFIG_PREEMPT_NOTIFIERS
#error "the kernel needs CONFIG_PREEMPT_NOTIFIERS (selected by CONFIG_KVM)"
#endif
#ifndef CONFIG_TRACEPOINTS
#error "the kernel needs CONFIG_TRACEPOINTS to learn of thread exit and exec"
#endif

struct rosemary_thread {
	struct preempt_notifier pn;
	u64 tpidrro;
};

static void rosemary_sched_in(struct preempt_notifier *pn, int cpu)
{
	write_sysreg(container_of(pn, struct rosemary_thread, pn)->tpidrro,
		     tpidrro_el0);
}

/* The next task's __switch_to sets its own TPIDRRO_EL0. */
static void rosemary_sched_out(struct preempt_notifier *pn,
			       struct task_struct *next)
{
}

static struct preempt_ops rosemary_ops = {
	.sched_in = rosemary_sched_in,
	.sched_out = rosemary_sched_out,
};

/* Only current changes its own notifier list, so reading it needs no lock. */
static struct rosemary_thread *rosemary_current(void)
{
	struct preempt_notifier *pn;

	hlist_for_each_entry(pn, &current->preempt_notifiers, link)
		if (pn->ops == &rosemary_ops)
			return container_of(pn, struct rosemary_thread, pn);
	return NULL;
}

/*
 * The scheduler walks current's list on this CPU, so it is changed only with
 * preemption off.
 */
static void rosemary_release(struct rosemary_thread *t)
{
	preempt_disable();
	preempt_notifier_unregister(&t->pn);
	write_sysreg(0, tpidrro_el0);
	preempt_enable();
	kfree(t);
	module_put(THIS_MODULE);
}

static long rosemary_set_tpidrro(u64 value)
{
	struct rosemary_thread *t = rosemary_current();

	if (!value) {
		if (t)
			rosemary_release(t);
		return 0;
	}
	if (!t) {
		t = kzalloc(sizeof(*t), GFP_KERNEL);
		if (!t)
			return -ENOMEM;
		/* Each live state pins the module, as its ops are in it. */
		if (!try_module_get(THIS_MODULE)) {
			kfree(t);
			return -ENODEV;
		}
		preempt_notifier_init(&t->pn, &rosemary_ops);
		preempt_disable();
		preempt_notifier_register(&t->pn);
	} else {
		preempt_disable();
	}
	t->tpidrro = value;
	write_sysreg(value, tpidrro_el0);
	preempt_enable();
	return 0;
}

static long rosemary_ioctl(struct file *file, unsigned int cmd,
			   unsigned long arg)
{
	void __user *user = (void __user *)arg;

	switch (cmd) {
	case ROSEMARY_IOC_VERSION: {
		__u32 version = ROSEMARY_ABI_VERSION;

		return copy_to_user(user, &version, sizeof(version)) ? -EFAULT : 0;
	}
	case ROSEMARY_IOC_INFO: {
		__u64 info = 0;

		if (arm64_kernel_unmapped_at_el0())
			info |= ROSEMARY_INFO_KPTI;
		return copy_to_user(user, &info, sizeof(info)) ? -EFAULT : 0;
	}
	case ROSEMARY_IOC_SET_TPIDRRO: {
		__u64 value;

		if (copy_from_user(&value, user, sizeof(value)))
			return -EFAULT;
		return rosemary_set_tpidrro(value);
	}
	default:
		return -ENOTTY;
	}
}

static const struct file_operations rosemary_fops = {
	.owner = THIS_MODULE,
	.unlocked_ioctl = rosemary_ioctl,
	.compat_ioctl = compat_ptr_ioctl,
};

/* The register is only ever the caller's own, so anyone may open the device. */
static struct miscdevice rosemary_device = {
	.minor = MISC_DYNAMIC_MINOR,
	.name = "rosemary",
	.fops = &rosemary_fops,
	.mode = 0666,
};

/*
 * A dying or exec'ing task drops its state; otherwise the notifier would stay
 * linked into a freed task_struct, or the value would leak into the new image.
 */
static void rosemary_forget(struct task_struct *p)
{
	struct rosemary_thread *t;

	if (p != current)
		return;
	t = rosemary_current();
	if (t)
		rosemary_release(t);
}

static void rosemary_on_exit(void *data, struct task_struct *p)
{
	rosemary_forget(p);
}

static void rosemary_on_exec(void *data, struct task_struct *p, pid_t old_pid,
			     struct linux_binprm *bprm)
{
	rosemary_forget(p);
}

struct rosemary_probe {
	const char *name;
	void *fn;
	struct tracepoint *tp;
};

static struct rosemary_probe rosemary_probes[] = {
	{ .name = "sched_process_exit", .fn = rosemary_on_exit },
	{ .name = "sched_process_exec", .fn = rosemary_on_exec },
};

/* These tracepoints are not exported by name. */
static void rosemary_find_tracepoint(struct tracepoint *tp, void *priv)
{
	size_t i;

	for (i = 0; i < ARRAY_SIZE(rosemary_probes); i++)
		if (!strcmp(tp->name, rosemary_probes[i].name))
			rosemary_probes[i].tp = tp;
}

static void rosemary_unregister_probes(size_t count)
{
	while (count--)
		tracepoint_probe_unregister(rosemary_probes[count].tp,
					    rosemary_probes[count].fn, NULL);
	tracepoint_synchronize_unregister();
}

static int __init rosemary_init(void)
{
	size_t i;
	int err;

	for_each_kernel_tracepoint(rosemary_find_tracepoint, NULL);
	for (i = 0; i < ARRAY_SIZE(rosemary_probes); i++) {
		if (!rosemary_probes[i].tp) {
			pr_err("rosemary: no %s tracepoint\n",
			       rosemary_probes[i].name);
			err = -ENOENT;
			goto unregister;
		}
		err = tracepoint_probe_register(rosemary_probes[i].tp,
						rosemary_probes[i].fn, NULL);
		if (err)
			goto unregister;
	}

	preempt_notifier_inc();
	err = misc_register(&rosemary_device);
	if (err) {
		preempt_notifier_dec();
		goto unregister;
	}
	return 0;

unregister:
	rosemary_unregister_probes(i);
	return err;
}

static void __exit rosemary_exit(void)
{
	misc_deregister(&rosemary_device);
	rosemary_unregister_probes(ARRAY_SIZE(rosemary_probes));
	preempt_notifier_dec();
}

module_init(rosemary_init);
module_exit(rosemary_exit);

MODULE_DESCRIPTION("Darwin-style TPIDRRO_EL0 for Rosemary guests");
MODULE_LICENSE("GPL");
