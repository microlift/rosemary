/* Declarations only - see freestanding.h.  A *handle*, as on Darwin: the
 * fixture passes `&t` around and the runtime is what turns it into a thread. */
#ifndef ROSETTA3_TEST_PTHREAD_H
#define ROSETTA3_TEST_PTHREAD_H

#include "stddef.h"

typedef struct r3_pthread *pthread_t;

R3_DECL int pthread_create(pthread_t *, const void *, void *(*)(void *), void *);
R3_DECL int pthread_join(pthread_t, void **);
R3_DECL int pthread_detach(pthread_t);
R3_DECL pthread_t pthread_self(void);
R3_DECL int pthread_equal(pthread_t, pthread_t);

#endif /* ROSETTA3_TEST_PTHREAD_H */
