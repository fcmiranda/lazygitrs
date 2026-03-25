#!/usr/bin/env bun

import { $ } from "bun";

const README_PATH = "README.md";
const START_MARKER = "<!-- GEN_BENCHMARKS_START -->";
const END_MARKER = "<!-- GEN_BENCHMARKS_END -->";

// Check dependencies
for (const cmd of ["hyperfine", "lazygitrs", "lazygit"]) {
  const which = await $`command -v ${cmd}`.quiet().nothrow();
  if (which.exitCode !== 0) {
    console.error(`Error: '${cmd}' not found`);
    process.exit(1);
  }
}

// Run benchmark
const bench =
  await $`hyperfine --warmup 5 -N 'lazygitrs --version' 'lazygit --version'`
    .text()
    .catch((e) => e.stdout?.toString() ?? e.message);

const block = `${START_MARKER}
### Benchmarks

Startup benchmark using [hyperfine](https://github.com/sharkdp/hyperfine):

\`\`\`sh

${bench.trim()}

\`\`\`
${END_MARKER}`;

// Read and replace
const readme = await Bun.file(README_PATH).text();

const startIdx = readme.indexOf(START_MARKER);
const endIdx = readme.indexOf(END_MARKER);

if (startIdx === -1 || endIdx === -1) {
  console.error(
    `Error: markers not found in ${README_PATH}.\nAdd ${START_MARKER} and ${END_MARKER} where you want benchmarks inserted.`
  );
  process.exit(1);
}

const before = readme.slice(0, startIdx);
const after = readme.slice(endIdx + END_MARKER.length);

await Bun.write(README_PATH, before + block + after);
console.log(`Benchmarks updated in ${README_PATH}`);
