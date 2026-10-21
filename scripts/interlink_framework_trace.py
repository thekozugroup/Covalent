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
SOURCE_SHA256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest()


class FrameworkTrace:
    """One read-only adb log stream, owned by its already-validated QA caller."""
    def __init__(self,adb,serial,framework):
        if not re.fullmatch(r"emulator-[0-9]+",serial):raise ValueError("Emulator transport required")
        self.command=[adb,"-s",serial,"logcat","-b","crash","-b","events","-b","system","-b","main","-v","threadtime","-T","1"]
        self.pids={int(framework[p]):p for p in ("system_server","surfaceflinger")}
        self.fatal_pids=set()
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
                "linesExamined":self.lines,"eventsDropped":self.dropped,"events":list(self.events)}

    def stop(self):
        if self.terminal is not None:return self.terminal
        before=self.snapshot();live=bool(self.proc and self.proc.poll() is None and self.thread.is_alive())
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
