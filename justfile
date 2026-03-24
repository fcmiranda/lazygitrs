default:
    just --list

ref-pull:
    bun scripts/fetch-references.ts pull

ref-clone:
    bun scripts/fetch-references.ts clone
