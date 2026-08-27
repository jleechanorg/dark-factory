/*
 * landlock_launcher.c — small kernel-enforced filesystem allow-list launcher.
 *
 * The parent process supplies paths that may be read and (separately) paths
 * that may be written.  Everything else is denied by Landlock before the
 * requested program is exec'd.  This is intentionally an executable rather
 * than an LD_PRELOAD hook: static binaries and direct syscalls remain subject
 * to the kernel rules.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/landlock.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/stat.h>
#include <unistd.h>

#ifndef __NR_landlock_create_ruleset
#if defined(__x86_64__)
#define __NR_landlock_create_ruleset 444
#define __NR_landlock_add_rule 445
#define __NR_landlock_restrict_self 446
#elif defined(__aarch64__)
#define __NR_landlock_create_ruleset 444
#define __NR_landlock_add_rule 445
#define __NR_landlock_restrict_self 446
#elif defined(__riscv)
#define __NR_landlock_create_ruleset 444
#define __NR_landlock_add_rule 445
#define __NR_landlock_restrict_self 446
#else
#error "unsupported architecture: define Landlock syscall numbers"
#endif
#endif

#ifndef LANDLOCK_CREATE_RULESET_VERSION
#define LANDLOCK_CREATE_RULESET_VERSION (1U << 0)
#endif
#ifndef LANDLOCK_RULE_PATH_BENEATH
#define LANDLOCK_RULE_PATH_BENEATH 1
#endif
#ifndef LANDLOCK_ACCESS_FS_EXECUTE
#define LANDLOCK_ACCESS_FS_EXECUTE (1ULL << 0)
#define LANDLOCK_ACCESS_FS_WRITE_FILE (1ULL << 1)
#define LANDLOCK_ACCESS_FS_READ_FILE (1ULL << 2)
#define LANDLOCK_ACCESS_FS_READ_DIR (1ULL << 3)
#define LANDLOCK_ACCESS_FS_REMOVE_DIR (1ULL << 4)
#define LANDLOCK_ACCESS_FS_REMOVE_FILE (1ULL << 5)
#define LANDLOCK_ACCESS_FS_MAKE_CHAR (1ULL << 6)
#define LANDLOCK_ACCESS_FS_MAKE_DIR (1ULL << 7)
#define LANDLOCK_ACCESS_FS_MAKE_REG (1ULL << 8)
#define LANDLOCK_ACCESS_FS_MAKE_SOCK (1ULL << 9)
#define LANDLOCK_ACCESS_FS_MAKE_FIFO (1ULL << 10)
#define LANDLOCK_ACCESS_FS_MAKE_BLOCK (1ULL << 11)
#define LANDLOCK_ACCESS_FS_MAKE_SYM (1ULL << 12)
#define LANDLOCK_ACCESS_FS_REFER (1ULL << 13)
#define LANDLOCK_ACCESS_FS_TRUNCATE (1ULL << 14)
#endif

struct path_rule {
    const char *path;
    int writable;
};

static void usage(const char *argv0) {
    fprintf(stderr, "usage: %s [--read PATH] [--write PATH] -- COMMAND [ARGS...]\n", argv0);
}

static int add_path_rule(int ruleset_fd, const struct path_rule *rule,
                         uint64_t read_access, uint64_t write_access) {
    int flags = O_PATH | O_CLOEXEC | O_NOFOLLOW;
    int path_fd = open(rule->path, flags);
    if (path_fd < 0) {
        return -1;
    }
    struct stat path_stat;
    if (fstat(path_fd, &path_stat) != 0) {
        int saved_errno = errno;
        close(path_fd);
        errno = saved_errno;
        return -1;
    }
    uint64_t allowed_access = rule->writable ? (read_access | write_access) : read_access;
    if (!S_ISDIR(path_stat.st_mode)) {
        allowed_access &= LANDLOCK_ACCESS_FS_EXECUTE |
                          LANDLOCK_ACCESS_FS_WRITE_FILE |
                          LANDLOCK_ACCESS_FS_READ_FILE;
    }
    struct landlock_path_beneath_attr path_beneath = {
        .parent_fd = path_fd,
        .allowed_access = allowed_access,
    };
    int rc = syscall(__NR_landlock_add_rule, ruleset_fd,
                     LANDLOCK_RULE_PATH_BENEATH, &path_beneath, 0);
    int saved_errno = errno;
    close(path_fd);
    errno = saved_errno;
    return rc;
}

int main(int argc, char **argv) {
    struct path_rule *rules = calloc((size_t)argc, sizeof(*rules));
    if (rules == NULL) {
        perror("calloc");
        return 125;
    }
    size_t rule_count = 0;
    int command_index = -1;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--") == 0) {
            command_index = i + 1;
            break;
        }
        if ((strcmp(argv[i], "--read") == 0 || strcmp(argv[i], "--write") == 0) &&
            i + 1 < argc && argv[i + 1][0] != '\0') {
            rules[rule_count].path = argv[i + 1];
            rules[rule_count].writable = strcmp(argv[i], "--write") == 0;
            rule_count++;
            i++;
            continue;
        }
        usage(argv[0]);
        free(rules);
        return 125;
    }
    if (command_index < 0 || command_index >= argc) {
        usage(argv[0]);
        free(rules);
        return 125;
    }

    int abi = syscall(__NR_landlock_create_ruleset, NULL, 0,
                      LANDLOCK_CREATE_RULESET_VERSION);
    if (abi < 1) {
        perror("landlock ABI unavailable");
        free(rules);
        return 125;
    }
    uint64_t read_access = LANDLOCK_ACCESS_FS_EXECUTE |
                           LANDLOCK_ACCESS_FS_READ_FILE |
                           LANDLOCK_ACCESS_FS_READ_DIR;
    uint64_t write_access = LANDLOCK_ACCESS_FS_WRITE_FILE |
                            LANDLOCK_ACCESS_FS_REMOVE_DIR |
                            LANDLOCK_ACCESS_FS_REMOVE_FILE |
                            LANDLOCK_ACCESS_FS_MAKE_CHAR |
                            LANDLOCK_ACCESS_FS_MAKE_DIR |
                            LANDLOCK_ACCESS_FS_MAKE_REG |
                            LANDLOCK_ACCESS_FS_MAKE_SOCK |
                            LANDLOCK_ACCESS_FS_MAKE_FIFO |
                            LANDLOCK_ACCESS_FS_MAKE_BLOCK |
                            LANDLOCK_ACCESS_FS_MAKE_SYM;
    if (abi >= 2) {
        write_access |= LANDLOCK_ACCESS_FS_REFER;
    }
    if (abi >= 3) {
        write_access |= LANDLOCK_ACCESS_FS_TRUNCATE;
    }
    struct landlock_ruleset_attr ruleset_attr = {
        .handled_access_fs = read_access | write_access,
    };
    int ruleset_fd = syscall(__NR_landlock_create_ruleset, &ruleset_attr,
                             sizeof(ruleset_attr), 0);
    if (ruleset_fd < 0) {
        perror("landlock_create_ruleset");
        free(rules);
        return 125;
    }
    for (size_t i = 0; i < rule_count; i++) {
        if (add_path_rule(ruleset_fd, &rules[i], read_access, write_access) != 0) {
            perror("landlock_add_rule");
            close(ruleset_fd);
            free(rules);
            return 125;
        }
    }
    free(rules);
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) {
        perror("PR_SET_NO_NEW_PRIVS");
        close(ruleset_fd);
        return 125;
    }
    if (syscall(__NR_landlock_restrict_self, ruleset_fd, 0) != 0) {
        perror("landlock_restrict_self");
        close(ruleset_fd);
        return 125;
    }
    close(ruleset_fd);
    execvp(argv[command_index], &argv[command_index]);
    perror("execvp");
    return errno == EACCES ? 126 : 127;
}
