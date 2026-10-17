"""Measure the real interactive Bash integration, with and without an existing trap."""
import subprocess
import shlex
import time


def run(executable, integration, enabled, env, cpu, samples):
    rows = []
    def pin():
        import os
        os.sched_setaffinity(0,{cpu})
    for existing in [False,True]:
        setup = "trap ':' DEBUG; " if existing else ""
        if enabled:
            setup += f"source {shlex.quote(str(integration))}; _rio_install_debug; _rio_prompt_app; "
        script = setup + "for ((i=0; i<10000; i++)); do :; done"
        name = "bash/command_loop" + ("_existing_hook" if existing else "")
        for sample in range(samples+1):
            start = time.perf_counter_ns()
            result = subprocess.run([executable,"--noprofile","--norc","-ic",script],env=env,
                                    stdout=subprocess.PIPE,stderr=subprocess.PIPE,preexec_fn=pin,timeout=20,check=True)
            ns = time.perf_counter_ns()-start
            reports = result.stdout.count(b"SetUserVar=RIO_APP=")
            if enabled and not 1<=reports<=3:
                raise RuntimeError(f"Expected at most one command report, got {reports} application reports including prompt setup")
            if sample:
                rows.append({"kind":"bench","case":name,"sample":sample-1,"ns":ns,"units":10000,"metric":"wall_ns_per_command_loop"})
        rows.append({"kind":"counter","case":name,"reports":reports})
    return rows
