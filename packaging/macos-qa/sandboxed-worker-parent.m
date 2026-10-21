// Test-only parent. Production executables retain their archived signatures.
#import <Foundation/Foundation.h>
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <unistd.h>

static int runChild(NSURL *binary, NSArray<NSString *> *arguments,
                    NSDictionary<NSString *, NSString *> *environment) {
  NSTask *task = [NSTask new];
  task.executableURL = binary;
  task.arguments = arguments;
  task.environment = environment;
  task.currentDirectoryURL = binary.URLByDeletingLastPathComponent;
  NSError *error = nil;
  if (![task launchAndReturnError:&error]) {
    printf("SANDBOX_WORKER_GATE launch_failed=true error_code=%ld\n", (long)error.code);
    return 66;
  }
  [task waitUntilExit];
  printf("SANDBOX_WORKER_GATE child_exit=%d child_signal=%s\n", task.terminationStatus,
         task.terminationReason == NSTaskTerminationReasonUncaughtSignal ? "true" : "false");
  fflush(stdout);
  return task.terminationReason == NSTaskTerminationReasonExit ? task.terminationStatus : 67;
}

int main(int argc, const char *argv[]) {
  @autoreleasepool {
    if (argc != 2) return 64;
    // The orchestrator creates and verifies this synthetic file before launch.
    // ENOENT is not evidence of a sandbox restriction.
    int descriptor = open(argv[1], O_RDONLY | O_NOFOLLOW);
    int savedError = errno;
    if (descriptor >= 0) close(descriptor);
    BOOL denied = descriptor < 0 && (savedError == EPERM || savedError == EACCES);
    NSString *home = NSHomeDirectory();
    NSString *temp = NSTemporaryDirectory();
    NSString *pattern = [temp stringByAppendingPathComponent:@"covalent-worker-probe-XXXXXX"];
    char *mutablePattern = strdup(pattern.fileSystemRepresentation);
    if (!mutablePattern) return 70;
    char *created = mkdtemp(mutablePattern);
    BOOL writable = created != NULL;
    if (created) rmdir(created);
    free(mutablePattern);
    printf("SANDBOX_WORKER_GATE outside_read_denied=%s private_temp_writable=%s\n",
           denied ? "true" : "false", writable ? "true" : "false");
    fflush(stdout);
    if (!denied || !writable) return 65;

    NSURL *executables = NSBundle.mainBundle.executableURL.URLByDeletingLastPathComponent;
    NSURL *worker = [executables URLByAppendingPathComponent:@"covalent-rclone"];
    NSDictionary *environment = @{
      @"HOME": home, @"TMPDIR": temp, @"PATH": @"/usr/bin:/bin:/usr/sbin:/sbin",
      @"COVALENT_TEST_SYNC_WORKER": worker.path,
      @"COVALENT_TEST_SYNC_GUARDIAN":
          [executables URLByAppendingPathComponent:@"covalent-engine-guardian"].path,
    };
    int workerResult = runChild(worker, @[@"version"], environment);
    if (workerResult) return workerResult;
    printf("SANDBOX_WORKER_GATE packaged_worker_started=true\n");
    fflush(stdout);
    return runChild([executables URLByAppendingPathComponent:@"one_way_links"],
                    @[@"--ignored", @"--exact",
                      @"manual_link_survives_both_endpoint_moves_and_retains_pairing", @"--nocapture"],
                    environment);
  }
}
