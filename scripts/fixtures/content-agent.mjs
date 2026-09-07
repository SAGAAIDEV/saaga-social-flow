// Deterministic test adapter: fail the first result, then repair on feedback.
import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";

let prompt = "";
for await (const chunk of process.stdin) prompt += chunk;
if (!prompt.includes("Build article components")) throw new Error("Expected stdin task");
if (process.cwd() !== process.env.CONTENT_JOB_DIR) throw new Error("Unexpected working directory");
const repaired = prompt.includes("Validation failed.");
const fixture = await readFile(new URL("../../examples/content-job/components.json", import.meta.url), "utf8");
await writeFile("components.json", repaired ? fixture : JSON.stringify({ version: 1, sourceId: "example:video-to-article", blocks: [] }));
await writeFile("REVIEW.md", "Test adapter: review required before publishing.");
console.log(repaired ? "Repaired result" : "Initial invalid result");
