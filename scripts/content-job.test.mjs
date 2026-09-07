import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, copyFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
const { safeLink, validateEmbed, validateValue } = await import(pathToFileURL(path.join(process.env.CONTENT_REPO_ROOT, "src/lib/content-workflow/validate.mjs")).href);
import { loadTopic, validateResult, main } from "./content-job.mjs";
import { codexArgs } from "./content-codex.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const example = path.join(root, "examples/content-job");
const catalog = JSON.parse(await readFile(path.join(process.env.CONTENT_REPO_ROOT, "src/components/organisms/content-blocks/component-catalog.json"), "utf8"));

test("Codex adapter uses stdin and preserves paths containing spaces as single arguments", () => {
  const args = codexArgs("/tmp/topic job", "/tmp/site checkout", "/tmp/cms checkout");
  assert.equal(args[args.indexOf("--cd") + 1], "/tmp/topic job");
  assert.equal(args[args.indexOf("--sandbox") + 1], "workspace-write");
  assert.equal(args.at(-1), "-");
  assert.ok(args.includes("/tmp/site checkout"));
  assert.ok(args.includes("/tmp/cms checkout"));
  assert.ok(!args.includes("--model"));
  assert.ok(!args.includes("--dangerously-bypass-approvals-and-sandbox"));
});

test("all catalog examples and legacy embeds validate", () => {
  for (const [componentKey, definition] of Object.entries(catalog)) {
    assert.deepEqual(validateEmbed({ __component: "content.embed", componentKey, componentVersion: definition.version, config: definition.example }, catalog), []);
  }
  assert.deepEqual(validateEmbed({ __component: "content.embed", componentKey: "model_comparison" }, catalog), []);
});

test("CMS cannot inject executable URLs or unrecognized config", () => {
  for (const href of ["javascript:alert(1)", "data:text/html,test", "//evil.test", "/\\evil.test", "https://user:pass@example.com", "\nhttps://example.com"]) assert.equal(safeLink(href), false, href);
  for (const href of ["/blog/test", "https://example.com/page", "#step-one"]) assert.equal(safeLink(href), true, href);
  assert.ok(validateEmbed({ __component: "content.embed", componentKey: "link_cards", config: { cards: [{ title: "Test", description: "Read", href: "javascript:alert(1)" }] } }, catalog).length);
  assert.ok(validateEmbed({ __component: "content.embed", componentKey: "step_cards", config: { steps: [], code: "alert(1)" } }, catalog).length);
});

test("unknown keys, prototype keys and unsupported versions fail", () => {
  for (const componentKey of ["missing", "constructor", "__proto__"]) assert.ok(validateEmbed({ __component: "content.embed", componentKey }, catalog).length);
  assert.ok(validateEmbed({ __component: "content.embed", componentKey: "model_comparison", componentVersion: 2 }, catalog).length);
  assert.ok(validateValue("test", { type: "not-supported" }).length);
});

test("one topic package produces validated blocks for all requested steps", async () => {
  const topic = await loadTopic(example);
  const result = await validateResult(example, topic);
  assert.equal(result.blocks.length, topic.steps.length);
});

test("handoff rejects missing steps, duplicates, and wrong source identity", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "content-job-test-"));
  const topic = await loadTopic(example);
  const original = JSON.parse(await readFile(path.join(example, "components.json"), "utf8"));
  for (const result of [
    { ...original, sourceId: "different" },
    { ...original, blocks: [] },
    { ...original, blocks: [original.blocks[0], original.blocks[0]] },
    { ...original, blocks: [null] },
    { ...original, blocks: [...original.blocks].reverse() },
  ]) {
    await writeFile(path.join(directory, "components.json"), JSON.stringify(result));
    await assert.rejects(validateResult(directory, topic));
  }
});

test("directory runner launches an adapter and repairs a rejected result", { timeout: 120000 }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "content-job-run-"));
  for (const name of ["topic.json", "transcript.txt"]) await copyFile(path.join(example, name), path.join(directory, name));
  await main(["run", directory, "--", process.execPath, path.join(root, "scripts/fixtures/content-agent.mjs")]);
  const report = JSON.parse(await readFile(path.join(directory, "validation.json"), "utf8"));
  assert.equal(report.status, "ready_for_review");
  assert.equal(report.attempt, 2);
  assert.match(await readFile(path.join(directory, "failure-1.log"), "utf8"), /Steps without components/);
  await assert.rejects(readFile(path.join(directory, ".content-job.lock")));
});

test("topic requires both media orientations and rejects transcript traversal", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "content-job-test-"));
  const topic = await loadTopic(example);
  await copyFile(path.join(example, "transcript.txt"), path.join(directory, "transcript.txt"));
  for (const invalid of [
    { ...topic, videos: { horizontal: "horizontal.mp4" } },
    { ...topic, transcript: "../outside.txt" },
    { ...topic, steps: [topic.steps[0], topic.steps[0]] },
  ]) {
    await writeFile(path.join(directory, "topic.json"), JSON.stringify(invalid));
    await assert.rejects(loadTopic(directory));
  }
});
