"""Bounded, allowlisted Android failure metadata. Never retains raw log lines."""
from collections import deque
import csv
import hashlib
import io
from pathlib import Path
import re
import subprocess
import threading
import time


PROCESSES=frozenset(("system_server","surfaceflinger","zygote","zygote64","netd","servicemanager",
    "com.android.systemui","com.android.settings","com.android.permissioncontroller",
    "com.google.android.permissioncontroller","life.michaelwong.interlink","life.michaelwong.interlink:node"))
EXCEPTIONS=("java.lang.OutOfMemoryError","java.lang.RuntimeException","java.lang.IllegalStateException",
    "java.lang.NullPointerException","java.lang.SecurityException","android.os.DeadObjectException",
    "android.os.DeadSystemException","android.os.DeadSystemRuntimeException","java.lang.IllegalArgumentException",
    "java.lang.NoSuchMethodError","java.lang.NoClassDefFoundError","java.lang.ClassNotFoundException","java.lang.AssertionError",
    "java.util.concurrent.TimeoutException")
PHASES=frozenset(("capture-started","input-tap-started","input-tap-returned","input-tap-timeout",
    "failure-observation-started","failure-observation-completed","cleanup-started","cleanup-completed",
    "exact debug APK and native host ready","revoked permission shows actionable recovery",
    "Settings return restarts native host and preserves identities",
    "runtime grant restarts native host and preserves identities",
    "Android framework remained stable through both recovery routes"))
PREFIX=re.compile(r"^(\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3})\s+(\d{1,10})\s+(\d{1,10})\s+([VDIWEF])\s+([A-Za-z0-9_$.-]{1,64})\s*: (.*)$")
START_SECONDS=5
NATIVE_DRAIN_SECONDS=5
# Fixed platform names only. Never emit a library path, thread name or arbitrary
# native symbol/message. Relative code offsets can identify a platform frame.
NATIVE_LIBRARIES=frozenset(("libc.so","libc++.so","libbase.so","liblog.so","libutils.so","libcutils.so",
    "libart.so","libartd.so","libandroid_runtime.so","libandroid_servers.so","libinputflinger.so","libinput.so",
    "libbinder.so","libhwbinder.so","libgui.so","libui.so","libsurfaceflinger.so","libEGL.so","libGLESv2.so",
    "libGLESv1_CM.so","libvulkan.so","libhwui.so","libnetd_client.so","libunwindstack.so","libopenjdk.so",
    "libjavacore.so","libgfxstream_backend.so","libOpenglRender.so","libEGL_emulation.so",
    "libGLESv2_emulation.so","libvulkan_ranchu.so","gralloc.ranchu.so","mapper.ranchu.so"))
NATIVE_SYMBOLS=frozenset(("abort","__assert2","__fortify_fatal","__stack_chk_fail","tgkill","pthread_kill",
    "raise","__android_log_assert","android::base::DefaultAborter(char const*)",
    "android::base::LogMessage::~LogMessage()","art::Runtime::Abort(char const*)"))
NATIVE_PATH=re.compile(r"/(?:system|system_ext|vendor|product)/(?:lib|lib64)/(?:[A-Za-z0-9_.-]+/){0,3}([A-Za-z0-9_.+-]+)"
    r"|/apex/com\.android\.[a-z0-9.]+/(?:lib|lib64)/(?:[A-Za-z0-9_.-]+/){0,3}([A-Za-z0-9_.+-]+)")
# Android37 also writes a bounded numeric parent PID between pid and tid.
# It is syntax only: correlation still uses the observed victim PID/TID/name,
# and the unrelated parent identifier never enters retained metadata.
NATIVE_HEADER=re.compile(r"pid: ([0-9]{1,10}), (?:ppid: [0-9]{1,10}, )?tid: ([0-9]{1,10}), name: .{0,256}  >>> ([^<>]{1,128}) <<<")
SOURCE_SHA256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest()


class NativeDumpDecoder:
    """Pure redacting decoder shared by the live stream and read-only reports."""
    def __init__(self):
        self.native_faults={};self.native_dumps={};self.native_frames_omitted=0

    def _native_dump(self,logger,payload,event):
        # crash_dump's logging PID differs from the crashed process. Attribute
        # only a header that matches a previously observed known-process signal.
        if payload.startswith("*** ***"):
            self.native_dumps.pop(logger,None);return None
        header=NATIVE_HEADER.fullmatch(payload)
        if payload.lstrip().startswith("pid:"):
            self.native_dumps.pop(logger,None)
            if not header:return None
        if header:
            victim=int(header[1]);process=header[3]
            if self.native_faults.get(victim)!=process:return None
            self.native_dumps[logger]={"victim":victim,"process":process,"frames":set()}
            if len(self.native_dumps)>16:self.native_dumps.pop(next(iter(self.native_dumps)))
            return {**event,"kind":"native-dump-header","victimPid":victim,"victimTid":int(header[2]),"process":process}
        context=self.native_dumps.get(logger)
        if context is None:return None
        event={**event,"victimPid":context["victim"],"process":context["process"]}
        if payload.startswith("Abort message: '") and payload.endswith("'"):
            message=payload[len("Abort message: '"):-1]
            families=(("FORTIFY:","fortify"),("Scudo ERROR:","allocator"),("fdsan:","descriptor-ownership"),
                ("stack corruption detected","stack-corruption"),("JNI DETECTED ERROR IN APPLICATION:","jni-contract"),
                ("Check failed:","assertion"),("terminating due to uncaught exception of type std::bad_alloc","native-allocation"))
            family=next((family for prefix,family in families if message.startswith(prefix)),"unclassified")
            if message=="Assertion failed: !rcEnc->featureInfo()->hasReadColorBufferDma":
                family="goldfish-read-color-buffer-dma"
            return {**event,"kind":"native-abort-summary","messageFamily":family,
                "messageSha256":hashlib.sha256(message.encode()).hexdigest(),"messageBytes":len(message.encode())}
        signal=re.match(r"signal ([0-9]{1,3}) \([A-Z0-9]+\), code (-?[0-9]{1,4}) \([A-Z0-9_]+\), fault addr ",payload)
        if signal:return {**event,"kind":"native-dump-signal","signal":int(signal[1]),"signalCode":int(signal[2])}
        frame=re.match(r"\s*#([0-9]{2,3})\s+pc\s+([a-fA-F0-9]{8,16})\s+(\S+)(.*)$",payload)
        if frame:
            index=int(frame[1]);path=NATIVE_PATH.fullmatch(frame[3])
            library=next((part for part in path.groups() if part),None) if path and not any(part in {".",".."} for part in frame[3].split("/")) else None
            if library not in NATIVE_LIBRARIES or index>=32 or index in context["frames"]:
                self.native_frames_omitted+=1;return None
            context["frames"].add(index)
            event.update(kind="native-frame",frame=index,library=library,pcOffset=int(frame[2],16))
            suffix=frame[4]
            build=re.search(r"\(BuildId: ([a-fA-F0-9]{8,64})\)\s*$",suffix)
            if build:event["buildId"]=build[1].lower();suffix=suffix[:build.start()].rstrip()
            symbol=re.fullmatch(r"\s+\((.+)\+[0-9]{1,10}\)",suffix)
            if symbol and symbol[1] in NATIVE_SYMBOLS:event["knownSymbol"]=symbol[1]
            return event
        return None


class FrameworkTrace(NativeDumpDecoder):
    """One read-only adb log stream, owned by its already-validated QA caller."""
    def __init__(self,adb,serial,framework):
        if not re.fullmatch(r"emulator-[0-9]+",serial):raise ValueError("Emulator transport required")
        self.command=[adb,"-s",serial,"logcat","-b","crash","-b","events","-b","system","-b","main","-v","threadtime","-T","1"]
        self.pids={int(framework[p]):p for p in ("system_server","surfaceflinger")}
        self.fatal_pids=set()
        super().__init__();self.native_drain_ms=0
        self.events=deque(maxlen=256);self.lock=threading.Lock();self.lines=0;self.dropped=0;self.error=None
        self.proc=None;self.thread=None;self.started=time.monotonic();self.terminal=None;self.attempted=False
        self.ready=threading.Event()

    def _remember(self,pid,process):
        self.pids[pid]=process
        if len(self.pids)>256:self.pids.pop(next(iter(self.pids)))

    def _append(self,event):
        with self.lock:
            if len(self.events)==self.events.maxlen:self.dropped+=1
            self.events.append({"hostElapsedMs":round((time.monotonic()-self.started)*1000),**event})

    def mark(self,phase,exit_code=None):
        if phase not in PHASES:raise ValueError("Unknown trace phase")
        event={"kind":"phase","name":phase}
        if exit_code is not None:event["exitCode"]=int(exit_code)
        self._append(event)

    def consume(self,line):
        """Accept only fixed classes, known processes and integer process metadata."""
        with self.lock:self.lines+=1
        if len(line)>8192:return
        match=PREFIX.fullmatch(line.rstrip("\r\n"))
        if not match:return
        stamp,pid,tid,level,tag,payload=match.groups();pid=int(pid)
        event={"deviceTime":stamp,"sourcePid":pid,"sourceTid":int(tid)}
        if tag in {"am_crash","am_anr","am_kill","am_proc_died","am_proc_start"}:
            if not payload.startswith("[") or not payload.endswith("]"):return
            try:fields=next(csv.reader(io.StringIO(payload[1:-1]),skipinitialspace=True))
            except (csv.Error,StopIteration):return
            index=3 if tag=="am_proc_start" else 2
            if len(fields)<=index or not re.fullmatch(r"[0-9]{1,10}",fields[1]):return
            victim=int(fields[1]);process=fields[index]
            if tag in {"am_proc_start","am_proc_died"}:self.fatal_pids.discard(victim)
            if process not in PROCESSES:
                if tag in {"am_proc_start","am_proc_died"}:self.pids.pop(victim,None)
                return
            if tag=="am_proc_died":self.pids.pop(victim,None)
            else:self._remember(victim,process)
            event.update(kind=tag,process=process,victimPid=victim)
            if tag=="am_crash":event["exceptionClasses"]=[fields[4]] if len(fields)>4 and fields[4] in EXCEPTIONS else []
        elif tag=="boot_progress_system_run" and re.fullmatch(r"[0-9]{1,16}",payload):
            self._remember(pid,"system_server");event.update(kind="framework-start",process="system_server")
        elif tag=="AndroidRuntime" and re.match(r"(?:\*\*\* )?FATAL EXCEPTION(?: IN SYSTEM PROCESS)?:",payload):
            self.fatal_pids.add(pid)
            if len(self.fatal_pids)>256:self.fatal_pids.remove(min(self.fatal_pids))
            event.update(kind="fatal-system-uid-process" if " IN SYSTEM PROCESS:" in payload else "fatal-application-process")
        elif tag=="Watchdog" and "WATCHDOG KILLING SYSTEM PROCESS" in payload:
            event.update(kind="framework-watchdog")
        elif tag=="lmkd":
            victim=re.search(r"Kill '([^']+)' \(([0-9]{1,10})\)",payload)
            if not victim or victim[1] not in PROCESSES:return
            event.update(kind="low-memory-kill",process=victim[1],victimPid=int(victim[2]))
        elif tag=="libc" and re.match(r"Fatal signal [0-9]{1,3}\b",payload):
            event.update(kind="native-fatal-signal",signal=int(re.match(r"Fatal signal ([0-9]+)",payload)[1]))
            if pid in self.pids:
                self.native_faults[pid]=self.pids[pid]
                if len(self.native_faults)>32:self.native_faults.pop(next(iter(self.native_faults)))
        elif tag=="DEBUG":
            event=self._native_dump(pid,payload,event)
            if event is None:return
        elif tag=="AndroidRuntime" and (pid in self.pids or pid in self.fatal_pids):
            classes=[e for e in EXCEPTIONS if re.match(r"\s*(?:Caused by: )?"+re.escape(e)+r"(?::|$)",payload)]
            families=[e for e in ("DeadSystemException","DeadSystemRuntimeException") if re.match(r"\s*(?:Caused by: )?"+e+r"(?::|$)",payload)]
            if not classes and not families:return
            event.update(kind="exception-class",exceptionClasses=classes)
            if families:event["unqualifiedExceptionNames"]=families
            if "java.util.concurrent.TimeoutException" in classes and "android.os.BinderProxy.finalize() timed out" in payload:
                event["fixedTimeoutComponent"]="android.os.BinderProxy.finalize"
        else:return
        if "process" not in event and pid in self.pids:event["process"]=self.pids[pid]
        self._append(event)

    def _read(self):
        try:
            discard=False
            while True:
                line=self.proc.stdout.readline(8193)
                if not line:break
                if len(line)>8192 or discard:
                    discard=not line.endswith("\n")
                    continue
                if PREFIX.fullmatch(line.rstrip("\r\n")) or line.rstrip() in {"--------- beginning of "+b for b in ("crash","events","system","main")}:
                    self.ready.set()
                self.consume(line)
        except (OSError,ValueError) as error:self.error=type(error).__name__

    def start(self):
        if self.attempted or self.terminal is not None:raise RuntimeError("Trace already started; never replace its stream")
        self.attempted=True
        self.started=time.monotonic()
        self.proc=subprocess.Popen(self.command,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,
            text=True,errors="replace",bufsize=1)
        self.thread=threading.Thread(target=self._read,name="interlink-framework-trace",daemon=True)
        self.thread.start()
        if not self.ready.wait(timeout=START_SECONDS):raise RuntimeError("Framework log stream did not become ready")
        self.mark("capture-started")

    def snapshot(self):
        if self.terminal is not None:return self.terminal
        with self.lock:
            return {"sourceSha256":SOURCE_SHA256,"sourceChanged":hashlib.sha256(Path(__file__).read_bytes()).hexdigest()!=SOURCE_SHA256,
                "rawLogsRetained":False,"mayContainPriorRecords":True,
                "streamStarted":self.proc is not None,"streamReady":self.ready.is_set(),"streamExitCode":self.proc.poll() if self.proc else None,
                "readerAlive":bool(self.thread and self.thread.is_alive()),"failureClass":self.error,
                "nativeFramesOmitted":self.native_frames_omitted,"nativeDetailDrainMs":self.native_drain_ms,
                "linesExamined":self.lines,"eventsDropped":self.dropped,"events":list(self.events)}

    def stop(self):
        if self.terminal is not None:return self.terminal
        before=self.snapshot();live=bool(self.proc and self.proc.poll() is None and self.thread.is_alive())
        if live and self.native_faults:
            # A native tombstone may follow the signal/failed command. Finish a
            # bounded read-only capture; this never retries input or restarts a guest.
            started=time.monotonic();deadline=started+NATIVE_DRAIN_SECONDS
            while self.proc.poll() is None and self.thread.is_alive() and time.monotonic()<deadline:
                time.sleep(min(0.05,max(0,deadline-time.monotonic())))
            self.native_drain_ms=round((time.monotonic()-started)*1000)
            live=bool(self.proc.poll() is None and self.thread.is_alive())
        if self.proc:
            if self.proc.poll() is None:
                self.proc.terminate()
                try:self.proc.wait(timeout=5)
                except subprocess.TimeoutExpired:self.proc.kill();self.proc.wait(timeout=2)
            if self.thread:self.thread.join(timeout=2)
            if self.proc.stdout and (not self.thread or not self.thread.is_alive()):self.proc.stdout.close()
        self.terminal={**self.snapshot(),"streamLiveBeforeStop":live,"readerAliveAfterStop":bool(self.thread and self.thread.is_alive()),"captureStopped":True}
        if not before["streamStarted"]:self.terminal["streamLiveBeforeStop"]=False
        return self.terminal
