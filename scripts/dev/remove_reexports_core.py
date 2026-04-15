#!/usr/bin/env python3
"""Remove all re-exports from uffs-core and fix callers."""

import re, os

ROOT = "crates/uffs-core/src"

# ─── Build type→submodule mapping ─────────────────────────────────────
REEXPORT_MAP = {}

def parse_reexports(filepath, parent_mod):
    with open(filepath) as f:
        content = f.read()
    for m in re.finditer(r'pub use (?:self::)?(\w+)::\{([^}]+)\};', content):
        submod = m.group(1)
        for item in m.group(2).split(','):
            item = item.strip()
            if item:
                REEXPORT_MAP[(parent_mod, item)] = submod
    for m in re.finditer(r'pub use (?:self::)?(\w+)::(\w+);', content):
        submod, item = m.group(1), m.group(2)
        if submod in ('uffs_mft', 'uffs_polars', 'uffs_client', 'crate'):
            continue  # skip cross-crate, handle separately
        REEXPORT_MAP[(parent_mod, item)] = submod

parse_reexports(f"{ROOT}/lib.rs", "")  # crate root
parse_reexports(f"{ROOT}/aggregate/mod.rs", "aggregate")
parse_reexports(f"{ROOT}/index_search/mod.rs", "index_search")
parse_reexports(f"{ROOT}/path_resolver/mod.rs", "path_resolver")
parse_reexports(f"{ROOT}/compiled_pattern/mod.rs", "compiled_pattern")
parse_reexports(f"{ROOT}/tree/mod.rs", "tree")

# search/backend.rs re-exports from super::sorting
REEXPORT_MAP[("search::backend", "dataframe_to_display_rows")] = "sorting"
REEXPORT_MAP[("search::backend", "sort_rows")] = "sorting"
REEXPORT_MAP[("search::backend", "SortOrder")] = "sorting"

print(f"Built map with {len(REEXPORT_MAP)} entries")

# ─── Delete re-export lines ──────────────────────────────────────────
REEXPORT_FILES = [
    f"{ROOT}/lib.rs",
    f"{ROOT}/aggregate/mod.rs",
    f"{ROOT}/index_search/mod.rs",
    f"{ROOT}/path_resolver/mod.rs",
    f"{ROOT}/compiled_pattern/mod.rs",
    f"{ROOT}/tree/mod.rs",
    f"{ROOT}/compact.rs",
    f"{ROOT}/search/backend.rs",
    f"{ROOT}/search/field/mod.rs",
    f"{ROOT}/search/filters/mod.rs",
]

deleted = 0
for fpath in REEXPORT_FILES:
    if not os.path.exists(fpath):
        continue
    with open(fpath) as f:
        lines = f.readlines()
    new_lines = []
    skip_multiline = False
    for line in lines:
        stripped = line.strip()
        if stripped.startswith('pub use self::') or \
           stripped.startswith('pub use super::') or \
           (stripped.startswith('pub use ') and '::' in stripped and
            not stripped.startswith('pub use std::') and
            not stripped.startswith('pub use rayon::')):
            deleted += 1
            if '{' in stripped and '}' not in stripped:
                skip_multiline = True
            continue
        if skip_multiline:
            if '}' in stripped:
                skip_multiline = False
            continue
        new_lines.append(line)
    with open(fpath, 'w') as f:
        f.writelines(new_lines)
print(f"Deleted {deleted} re-export lines")

# ─── Rewrite callers within uffs-core ─────────────────────────────────
REPLACEMENTS = []
for (parent, item), submod in sorted(REEXPORT_MAP.items(), key=lambda x: -len(x[0][1])):
    if parent == "":
        # crate root re-exports: crate::Item → crate::submod::Item
        old = f"crate::{item}"
        new = f"crate::{submod}::{item}"
    else:
        old = f"crate::{parent}::{item}"
        new = f"crate::{parent}::{submod}::{item}"
    REPLACEMENTS.append((old, new))

# Build super:: mapping per parent
SUPER_MAP = {}
for (parent, item), submod in REEXPORT_MAP.items():
    if parent not in SUPER_MAP:
        SUPER_MAP[parent] = []
    SUPER_MAP[parent].append((item, submod))

def fix_file(fpath):
    with open(fpath) as f:
        content = f.read()
    original = content
    for old, new in REPLACEMENTS:
        content = content.replace(old, new)
    # super:: fixes for submodule files
    rel = os.path.relpath(fpath, ROOT)
    parts = rel.replace('\\', '/').split('/')
    parent_ctx = None
    if len(parts) >= 2:
        p = parts[0]
        if p in ('aggregate','index_search','path_resolver','compiled_pattern','tree','compact'):
            parent_ctx = p
        elif p == 'search' and len(parts) >= 3:
            if parts[1] in ('backend','filters','field'):
                parent_ctx = f'search::{parts[1]}'
    if parent_ctx and parent_ctx in SUPER_MAP:
        for item, submod in sorted(SUPER_MAP[parent_ctx], key=lambda x: -len(x[0])):
            content = content.replace(f"super::{item}", f"super::{submod}::{item}")
    if content != original:
        with open(fpath, 'w') as f:
            f.write(content)
        return True
    return False

changed = 0
for root_dir, dirs, files in os.walk(ROOT):
    for fname in files:
        if fname.endswith('.rs'):
            if fix_file(os.path.join(root_dir, fname)):
                changed += 1
print(f"Updated {changed} files")
