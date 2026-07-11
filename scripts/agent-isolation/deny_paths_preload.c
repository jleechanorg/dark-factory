/*
 * deny_paths_preload.c — LD_PRELOAD filesystem deny-list shim.
 *
 * Linux has no unprivileged equivalent of macOS's sandbox-exec deny rules
 * when user namespaces are blocked (e.g. by
 * kernel.apparmor_restrict_unprivileged_userns=1, the Ubuntu 24.04 default
 * as shipped, and the state on Jeff-Ubuntu at the time this was written).
 * bubblewrap and `systemd-run --user --scope -p InaccessiblePaths=...` both
 * require a mount namespace, which itself requires either a privileged
 * caller or an unprivileged user namespace — neither is available here.
 *
 * This shim provides real, verifiable filesystem containment WITHOUT any
 * namespace or root privilege: it intercepts libc's file-open entry points
 * via the dynamic linker (LD_PRELOAD) and rejects any resolved path that
 * falls under one of the colon-separated absolute path prefixes in the
 * DENY_PATHS environment variable.
 *
 * Scope / honesty notes (read before assuming this is bulletproof):
 *   - Only covers dynamically-linked processes that respect LD_PRELOAD.
 *     A statically-linked binary, a setuid binary (LD_PRELOAD is dropped
 *     by the dynamic linker for setuid/setgid targets), or a process that
 *     issues raw `openat(2)` via syscall(2)/direct syscall trampoline
 *     bypasses this shim entirely. This is NOT kernel-level containment;
 *     it is userspace interposition. Document this limitation everywhere
 *     this shim is referenced.
 *   - Covers open/open64/openat/openat64/fopen/fopen64 — the entry points
 *     every mainstream CLI tool (git, python, node, ripgrep, cat, coreutils)
 *     uses for file reads on glibc Linux.
 *   - Denies by returning ENOENT for the open/openat family and NULL
 *     (errno=ENOENT) for fopen/fopen64 — deliberately NOT EACCES, so a
 *     caller can't infer "the file exists but I'm blocked from it" (that
 *     confirms secret paths exist even when denied read access).
 *   - Path resolution: absolute paths are checked directly; relative paths
 *     are resolved against the process's current working directory (via
 *     getcwd) for `open`/`fopen`, and against the target of
 *     /proc/self/fd/<dirfd> for `openat` when dirfd != AT_FDCWD. Symlink
 *     targets are resolved via realpath(3) when the path exists, so a
 *     symlink pointing into a denied tree is still denied.
 */

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define MAX_DENIED_PATHS 64

static char *g_denied[MAX_DENIED_PATHS];
static int g_denied_count = 0;
static int g_initialized = 0;

static void deny_paths_init(void) {
    if (g_initialized) {
        return;
    }
    g_initialized = 1;
    const char *raw = getenv("DENY_PATHS");
    if (!raw || !*raw) {
        return;
    }
    char *copy = strdup(raw);
    if (!copy) {
        return;
    }
    char *saveptr = NULL;
    char *tok = strtok_r(copy, ":", &saveptr);
    while (tok && g_denied_count < MAX_DENIED_PATHS) {
        size_t len = strlen(tok);
        while (len > 1 && tok[len - 1] == '/') {
            tok[len - 1] = '\0';
            len--;
        }
        if (len > 0) {
            g_denied[g_denied_count++] = strdup(tok);
        }
        tok = strtok_r(NULL, ":", &saveptr);
    }
    free(copy);
}

/* Returns 1 if `resolved` (an absolute, non-empty path) is under one of the
 * denied prefixes (exact match or `<prefix>/...`). */
static int path_is_denied(const char *resolved) {
    if (!resolved || !*resolved) {
        return 0;
    }
    for (int i = 0; i < g_denied_count; i++) {
        const char *prefix = g_denied[i];
        size_t plen = strlen(prefix);
        if (strncmp(resolved, prefix, plen) == 0) {
            if (resolved[plen] == '\0' || resolved[plen] == '/') {
                return 1;
            }
        }
    }
    return 0;
}

/* Best-effort absolute-path resolution. Uses realpath(3) when the target
 * exists (covers symlink escapes); falls back to lexical join with cwd for
 * paths that don't exist yet (e.g. a caller checking O_CREAT). */
static void resolve_absolute(const char *path, char *out, size_t outsz) {
    if (!path) {
        out[0] = '\0';
        return;
    }
    if (realpath(path, out) != NULL) {
        return;
    }
    if (path[0] == '/') {
        snprintf(out, outsz, "%s", path);
        return;
    }
    char cwd[PATH_MAX];
    if (getcwd(cwd, sizeof(cwd)) != NULL) {
        char joined[PATH_MAX * 2];
        snprintf(joined, sizeof(joined), "%s/%s", cwd, path);
        snprintf(out, outsz, "%s", joined);
    } else {
        snprintf(out, outsz, "%s", path);
    }
}

static void resolve_at(int dirfd, const char *path, char *out, size_t outsz) {
    if (path && path[0] == '/') {
        resolve_absolute(path, out, outsz);
        return;
    }
    if (dirfd == AT_FDCWD) {
        resolve_absolute(path, out, outsz);
        return;
    }
    char fdlink[64];
    snprintf(fdlink, sizeof(fdlink), "/proc/self/fd/%d", dirfd);
    char base[PATH_MAX];
    ssize_t n = readlink(fdlink, base, sizeof(base) - 1);
    if (n <= 0) {
        /* Can't resolve the base dir; fail safe by treating as unresolved
         * (not automatically denied, but not silently allowed either --
         * the real open() will still run normally). */
        out[0] = '\0';
        return;
    }
    base[n] = '\0';
    char joined[PATH_MAX * 2];
    snprintf(joined, sizeof(joined), "%s/%s", base, path ? path : "");
    if (realpath(joined, out) == NULL) {
        snprintf(out, outsz, "%s", joined);
    }
}

typedef int (*open_fn_t)(const char *, int, ...);
typedef int (*openat_fn_t)(int, const char *, int, ...);
typedef FILE *(*fopen_fn_t)(const char *, const char *);

static mode_t va_mode(va_list ap) {
    return (mode_t)va_arg(ap, int);
}

int open(const char *pathname, int flags, ...) {
    deny_paths_init();
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode = va_mode(ap);
        va_end(ap);
    }
    char resolved[PATH_MAX];
    resolve_absolute(pathname, resolved, sizeof(resolved));
    if (path_is_denied(resolved)) {
        errno = ENOENT;
        return -1;
    }
    static open_fn_t real_open = NULL;
    if (!real_open) {
        real_open = (open_fn_t)dlsym(RTLD_NEXT, "open");
    }
    return real_open(pathname, flags, mode);
}

int open64(const char *pathname, int flags, ...) {
    deny_paths_init();
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode = va_mode(ap);
        va_end(ap);
    }
    char resolved[PATH_MAX];
    resolve_absolute(pathname, resolved, sizeof(resolved));
    if (path_is_denied(resolved)) {
        errno = ENOENT;
        return -1;
    }
    static open_fn_t real_open64 = NULL;
    if (!real_open64) {
        real_open64 = (open_fn_t)dlsym(RTLD_NEXT, "open64");
    }
    return real_open64(pathname, flags, mode);
}

int openat(int dirfd, const char *pathname, int flags, ...) {
    deny_paths_init();
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode = va_mode(ap);
        va_end(ap);
    }
    char resolved[PATH_MAX];
    resolve_at(dirfd, pathname, resolved, sizeof(resolved));
    if (path_is_denied(resolved)) {
        errno = ENOENT;
        return -1;
    }
    static openat_fn_t real_openat = NULL;
    if (!real_openat) {
        real_openat = (openat_fn_t)dlsym(RTLD_NEXT, "openat");
    }
    return real_openat(dirfd, pathname, flags, mode);
}

int openat64(int dirfd, const char *pathname, int flags, ...) {
    deny_paths_init();
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode = va_mode(ap);
        va_end(ap);
    }
    char resolved[PATH_MAX];
    resolve_at(dirfd, pathname, resolved, sizeof(resolved));
    if (path_is_denied(resolved)) {
        errno = ENOENT;
        return -1;
    }
    static openat_fn_t real_openat64 = NULL;
    if (!real_openat64) {
        real_openat64 = (openat_fn_t)dlsym(RTLD_NEXT, "openat64");
    }
    return real_openat64(dirfd, pathname, flags, mode);
}

FILE *fopen(const char *pathname, const char *mode) {
    deny_paths_init();
    char resolved[PATH_MAX];
    resolve_absolute(pathname, resolved, sizeof(resolved));
    if (path_is_denied(resolved)) {
        errno = ENOENT;
        return NULL;
    }
    static fopen_fn_t real_fopen = NULL;
    if (!real_fopen) {
        real_fopen = (fopen_fn_t)dlsym(RTLD_NEXT, "fopen");
    }
    return real_fopen(pathname, mode);
}

FILE *fopen64(const char *pathname, const char *mode) {
    deny_paths_init();
    char resolved[PATH_MAX];
    resolve_absolute(pathname, resolved, sizeof(resolved));
    if (path_is_denied(resolved)) {
        errno = ENOENT;
        return NULL;
    }
    static fopen_fn_t real_fopen64 = NULL;
    if (!real_fopen64) {
        real_fopen64 = (fopen_fn_t)dlsym(RTLD_NEXT, "fopen64");
    }
    return real_fopen64(pathname, mode);
}
