#!/usr/bin/env node
import { spawn } from "node:child_process";
import { realpath } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

/** Uses the caller's Codex login/model configuration with bounded writable roots. */
export function codexArgs(job, repo, cms) {
  return ["exec", "--cd", job, "--skip-git-repo-check", "--sandbox", "workspace-write",
    "-c", 'approval_policy="never"', "--add-dir", repo,
    ...(cms ? ["--add-dir", cms] : []), "--color", "never", "-"];
}

export async function main() {
  if (!process.env.CONTENT_JOB_DIR || !process.env.CONTENT_REPO_ROOT) throw new Error("Launch through content:job run; job and repository paths are required");
  const job = await realpath(process.env.CONTENT_JOB_DIR);
  const repo = await realpath(process.env.CONTENT_REPO_ROOT);
  const cms = process.env.CONTENT_CMS_ROOT ? await realpath(process.env.CONTENT_CMS_ROOT) : undefined;
  const child = spawn(process.env.CONTENT_CODEX_BIN || "codex", codexArgs(job, repo, cms), {
    cwd: job, shell: false, stdio: "inherit",
  });
  // The outer job runner's timeout must reach the actual coding process.
  const terminate = () => child.kill("SIGTERM");
  process.once("SIGTERM", terminate);
  process.once("SIGINT", terminate);
  await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code) => code === 0 ? resolve() : reject(new Error(`Codex exited ${code}`)));
  }).finally(() => {
    process.removeListener("SIGTERM", terminate);
    process.removeListener("SIGINT", terminate);
  });
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1; });
}
