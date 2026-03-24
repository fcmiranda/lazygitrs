import { $ } from "bun";
import { appendFileSync, existsSync, readdirSync, writeFileSync } from "fs";
import { join } from "path";

// ============================================================================ //
//                                    SOURCES                                   //
// ============================================================================ //
const sources = [
	{
		repo: "jesseduffield/lazygit",
		branch: "master",
		dir: "_tmp_lazygit",
	},
	{
		repo: "jnsahaj/lumen",
		branch: "main",
		dir: "_tmp_lumen",
	},
];

function isNonEmptyDir(dir: string): boolean {
	if (!existsSync(dir)) return false;
	try {
		return readdirSync(dir).length > 0;
	} catch {
		return false;
	}
}

async function gitClone(
	repo: string,
	branch: string,
	subdir: string | undefined,
	dir: string,
) {
	const url = `https://github.com/${repo}.git`;
	if (subdir) {
		await $`git clone --depth 1 --branch ${branch} --filter=blob:none --sparse ${url} ${dir}`;
		await $`cd ${dir} && git sparse-checkout set ${subdir}`;
	} else {
		await $`git clone --depth 1 --branch ${branch} ${url} ${dir}`;
	}
}

async function gitPull(dir: string) {
	await $`cd ${dir} && git pull`;
}

async function init() {
	const ignoreFile = ".ignore";
	const gitignoreFile = ".gitignore";
	const ignoreContent = "!_tmp_*\n";
	const gitignoreContent = "_tmp_*\n";

	if (existsSync(ignoreFile)) {
		const content = await Bun.file(ignoreFile).text();
		if (!content.includes("!_tmp_*")) {
			appendFileSync(ignoreFile, ignoreContent);
			console.log(`✓ Appended to ${ignoreFile}`);
		} else {
			console.log(`✓ ${ignoreFile} already configured`);
		}
	} else {
		writeFileSync(ignoreFile, ignoreContent);
		console.log(`✓ Created ${ignoreFile}`);
	}

	if (existsSync(gitignoreFile)) {
		const content = await Bun.file(gitignoreFile).text();
		if (!content.includes("_tmp_*")) {
			appendFileSync(gitignoreFile, gitignoreContent);
			console.log(`✓ Appended to ${gitignoreFile}`);
		} else {
			console.log(`✓ ${gitignoreFile} already configured`);
		}
	} else {
		writeFileSync(gitignoreFile, gitignoreContent);
		console.log(`✓ Created ${gitignoreFile}`);
	}
}

async function clone() {
	await init();
	for (const source of sources) {
		const { repo, branch, subdir, dir } = source;
		if (isNonEmptyDir(dir)) {
			console.log(`✓ ${dir} already exists, skipping`);
			continue;
		}
		console.log(`↓ Cloning ${repo} (${branch}) to ${dir}...`);
		await gitClone(repo, branch, subdir, dir);
		console.log(`✓ Cloned ${dir}`);
	}
}

async function pull() {
	for (const source of sources) {
		const { repo, dir } = source;
		if (!isNonEmptyDir(dir)) {
			console.log(`✗ ${dir} doesn't exist, run 'clone' first`);
			continue;
		}
		console.log(`↓ Pulling ${repo} in ${dir}...`);
		await gitPull(dir);
		console.log(`✓ Pulled ${dir}`);
	}
}

const command = process.argv[2];

async function main() {
	switch (command) {
		case "clone":
			await clone();
			break;
		case "pull":
			await pull();
			break;
		default:
			console.log("Usage: bun scripts/fetch-references.ts [clone|pull]");
			console.log("  clone - Clone reference repos (also runs init)");
			console.log("  pull  - Pull latest changes for cloned repos");
			process.exit(1);
	}
}

main().catch(console.error);
