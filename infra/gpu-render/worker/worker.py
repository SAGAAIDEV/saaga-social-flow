#!/usr/bin/env python3
"""One render machine's whole life: install, fetch, render on the GPU, upload.

Launched by the user data src/edit/gpu.rs writes, with the bucket, the run and
this machine's shard. Standard library only, and S3 through the AWS CLI the AMI
ships with, so nothing has to be installed before the first status goes up.

The protocol is files in the bucket:
  runs/<run>/manifest.json         read: settings, every job's inputs, the shards
  runs/<run>/status/<shard>.json   written every few seconds — the app's only view
  runs/<run>/out/<job>.mp4         written per finished job
  runs/<run>/logs/<job>.log        written per job, finished or not

Exits when its jobs are done or failed; the user data then powers the machine
off, which the launch template turns into a terminate.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import threading
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path("/render")
BLOBS = ROOT / "blobs"
JOBS = ROOT / "jobs"
PERCENT = re.compile(r"(\d{1,3})%\s+(.*)")
FRAMES = re.compile(r"frame (\d+)/(\d+)")


def aws(*args, stdin=None):
    return subprocess.run(
        ["aws", *args], input=stdin, check=True, capture_output=True
    ).stdout


def imds(path):
    """One instance-metadata value, over IMDSv2 (the launch template requires it)."""
    token_req = urllib.request.Request(
        "http://169.254.169.254/latest/api/token",
        method="PUT",
        headers={"X-aws-ec2-metadata-token-ttl-seconds": "300"},
    )
    token = urllib.request.urlopen(token_req, timeout=2).read().decode()
    req = urllib.request.Request(
        f"http://169.254.169.254/latest/meta-data/{path}",
        headers={"X-aws-ec2-metadata-token": token},
    )
    return urllib.request.urlopen(req, timeout=2).read().decode()


class Status:
    """This machine's status file, rewritten on change and at least every 10 s."""

    def __init__(self, bucket, run, shard, jobs):
        self.key = f"s3://{bucket}/runs/{run}/status/{shard}.json"
        self.lock = threading.Lock()
        self.dirty = threading.Event()
        try:
            instance = imds("instance-id")
        except Exception:
            instance = None
        self.doc = {
            "shard": shard,
            "instance": instance,
            "phase": "installing",
            "message": "Installing Chrome, ffmpeg and hyperframes",
            "gpu": None,
            "started": time.time(),
            "updated": time.time(),
            "jobs": {job: {"state": "queued", "progress": 0.0} for job in jobs},
        }
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._loop, daemon=True)
        self.thread.start()
        self.dirty.set()

    def set(self, **fields):
        with self.lock:
            self.doc.update(fields)
        self.dirty.set()

    def job(self, job, **fields):
        with self.lock:
            self.doc["jobs"][job].update(fields)
        self.dirty.set()

    def _write(self):
        with self.lock:
            self.doc["updated"] = time.time()
            body = json.dumps(self.doc).encode()
        try:
            aws("s3", "cp", "-", self.key, "--content-type", "application/json", stdin=body)
        except subprocess.CalledProcessError as err:
            print("status write failed:", err.stderr.decode(errors="replace"), flush=True)

    def _loop(self):
        while not self.stop.is_set():
            self.dirty.wait(timeout=10)
            self.dirty.clear()
            self._write()
            time.sleep(2)

    def close(self):
        self.stop.set()
        self.dirty.set()
        self.thread.join(timeout=15)
        self._write()


def install(settings):
    env = dict(os.environ, HF_VERSION=settings["hf_version"], CHROME_VERSION=settings["chrome"])
    with open("/var/log/render-setup.log", "wb") as log:
        subprocess.run(
            ["bash", str(ROOT / "setup.sh")], env=env, stdout=log, stderr=subprocess.STDOUT, check=True
        )
    gpu = subprocess.run(
        ["nvidia-smi", "--query-gpu=name", "--format=csv,noheader"], capture_output=True, text=True
    ).stdout.strip()
    # The binary, not the folder of the same name @puppeteer/browsers puts it in:
    # /opt/chrome/chrome-headless-shell/linux-<v>/chrome-headless-shell-linux64/chrome-headless-shell
    shell = next(
        (p for p in Path("/opt/chrome").rglob("chrome-headless-shell")
         if p.is_file() and os.access(p, os.X_OK)),
        None,
    )
    if shell is None:
        raise RuntimeError("chrome-headless-shell did not install")
    return gpu, str(shell)


def fetch(bucket, digests):
    BLOBS.mkdir(parents=True, exist_ok=True)

    def one(digest):
        dest = BLOBS / digest
        if not dest.exists():
            part = dest.with_suffix(".part")
            aws("s3", "cp", f"s3://{bucket}/blobs/{digest}", str(part), "--only-show-errors")
            # The key is the content's sha256; a blob that is not what its name
            # says is never rendered from.
            if sha256_of(part) != digest:
                part.unlink(missing_ok=True)
                raise RuntimeError(f"blobs/{digest} does not match its own digest")
            part.rename(dest)

    with ThreadPoolExecutor(max_workers=16) as pool:
        list(pool.map(one, sorted(digests)))


def sha256_of(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def lay_out(job):
    """The job's project: its files, hard-linked from the blob store."""
    root = JOBS / job["id"]
    if root.exists():
        shutil.rmtree(root)
    for rel, digest in job["files"].items():
        dest = (root / rel).resolve()
        if root.resolve() not in dest.parents:
            raise RuntimeError(f"{rel!r} is outside the job's project")
        dest.parent.mkdir(parents=True, exist_ok=True)
        os.link(BLOBS / digest, dest)
    return root


def render(job, settings, shell, status, bucket, run):
    jid = job["id"]
    root = lay_out(job)
    out = root.parent / f"{jid}.mp4"
    log_path = root.parent / f"{jid}.log"
    cmd = [
        "hyperframes", "render",
        "--quality", settings["quality"],
        "--crf", settings["crf"],
        "--video-frame-format", settings["frame_format"],
        "--resolution", job["resolution"],
        "--workers", str(settings["workers"]),
        "--browser-gpu",
        "--output", str(out),
    ]
    env = dict(
        os.environ,
        PRODUCER_HEADLESS_SHELL_PATH=shell,
        HOME="/root",
        PATH="/usr/local/bin:" + os.environ.get("PATH", "/usr/bin:/bin"),
    )
    status.job(jid, state="rendering", started=time.time(), progress=0.0)
    started = time.time()
    last_change = time.time()
    last_progress = None
    error = None
    with open(log_path, "wb") as log:
        proc = subprocess.Popen(cmd, cwd=root, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        buf = b""
        stall = settings["stall_seconds"]

        # A render that stops producing progress is killed rather than left to
        # run until the machine's own deadline: silence is not success.
        def watchdog():
            while proc.poll() is None:
                if time.time() - last_change > stall:
                    proc.kill()
                    return
                time.sleep(5)

        threading.Thread(target=watchdog, daemon=True).start()
        while True:
            chunk = proc.stdout.read1(65536)
            if not chunk:
                break
            log.write(chunk)
            buf += chunk
            *lines, buf = re.split(rb"[\r\n]", buf)
            for raw in lines:
                line = raw.decode(errors="replace")
                if "browserGpuMode probe" in line and "software" in line:
                    error = "Chrome fell back to software rendering — no GPU in use"
                    proc.kill()
                match = PERCENT.search(line)
                if match:
                    progress = min(int(match.group(1)), 100) / 100
                    frames = FRAMES.search(line)
                    step = (progress, frames.group(0) if frames else match.group(2).strip()[:60])
                    if step != last_progress:
                        last_progress = step
                        last_change = time.time()
                        status.job(jid, progress=progress, step=step[1])
        code = proc.wait()
    aws("s3", "cp", str(log_path), f"s3://{bucket}/runs/{run}/logs/{jid}.log", "--only-show-errors")
    if code != 0 or not out.is_file() or out.stat().st_size == 0:
        if error is None and time.time() - last_change > settings["stall_seconds"]:
            error = f"no progress for {settings['stall_seconds']} s — killed"
        tail = [l for l in re.split(r"[\r\n]", log_path.read_text(errors="replace")) if l.strip()][-4:]
        raise RuntimeError(error or f"hyperframes exited {code}: " + " | ".join(tail))
    status.job(jid, state="uploading", progress=1.0)
    aws("s3", "cp", str(out), f"s3://{bucket}/runs/{run}/out/{jid}.mp4", "--only-show-errors")
    status.job(jid, state="done", finished=time.time(), seconds=round(time.time() - started, 1))
    shutil.rmtree(root, ignore_errors=True)
    out.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bucket", required=True)
    parser.add_argument("--run", required=True)
    parser.add_argument("--shard", type=int, required=True)
    args = parser.parse_args()

    ROOT.mkdir(parents=True, exist_ok=True)
    manifest = json.loads(aws("s3", "cp", f"s3://{args.bucket}/runs/{args.run}/manifest.json", "-"))
    settings = manifest["settings"]
    mine = set(manifest["shards"][args.shard])
    jobs = [job for job in manifest["jobs"] if job["id"] in mine]
    status = Status(args.bucket, args.run, args.shard, [job["id"] for job in jobs])
    try:
        for name in ("setup.sh", "package.json", "package-lock.json"):
            aws("s3", "cp", f"s3://{args.bucket}/worker/{name}", str(ROOT / name))
        # Install and download at once: both are minutes, and neither needs
        # the other.
        digests = {d for job in jobs for d in job["files"].values()}
        with ThreadPoolExecutor(max_workers=2) as pool:
            installing = pool.submit(install, settings)
            fetching = pool.submit(fetch, args.bucket, digests)
            gpu, shell = installing.result()
            fetching.result()
        status.set(phase="rendering", message=f"Rendering on {gpu or 'an unknown GPU'}", gpu=gpu)

        def guarded(job):
            try:
                render(job, settings, shell, status, args.bucket, args.run)
            except Exception as err:
                status.job(job["id"], state="failed", error=str(err)[:600], finished=time.time())

        # Longest first, so the last job to start is a short one.
        jobs.sort(key=lambda job: -job.get("weight", 0))
        with ThreadPoolExecutor(max_workers=settings["concurrency"]) as pool:
            list(pool.map(guarded, jobs))
        failed = [j for j, s in status.doc["jobs"].items() if s["state"] == "failed"]
        status.set(
            phase="failed" if failed else "done",
            message=f"{len(failed)} job(s) failed" if failed else "All jobs rendered",
        )
    except Exception as err:
        status.set(phase="failed", message=f"{type(err).__name__}: {err}"[:800])
        for jid, job in status.doc["jobs"].items():
            if job["state"] not in ("done", "failed"):
                status.job(jid, state="failed", error="the machine failed before this job ran")
        try:
            aws("s3", "cp", "/var/log/render-setup.log",
                f"s3://{args.bucket}/runs/{args.run}/logs/setup-{args.shard}.log", "--only-show-errors")
        except Exception:
            pass
    finally:
        status.close()


if __name__ == "__main__":
    main()
