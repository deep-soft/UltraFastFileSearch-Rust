#!/usr/bin/env python3
"""Remove all re-exports from uffs-mft and fix callers."""

import re, os, glob

ROOT = "crates/uffs-mft/src"

# ─── Step 1: Build the type→submodule mapping ─────────────────────────
# Maps (parent_module, type_name) → submodule
# e.g. ("index", "MftIndex") → "model"
REEXPORT_MAP = {}

def parse_reexports(filepath, parent_mod):
    """Parse pub use self::sub::{A, B} and pub use sub::{A, B} lines."""
    with open(filepath) as f:
        content = f.read()
    # Match: pub use [self::]submod::{Item1, Item2};
    for m in re.finditer(r'pub use (?:self::)?(\w+)::\{([^}]+)\};', content):
        submod = m.group(1)
        if submod in ('std', 'rayon', 'tracing', 'windows', 'polars'):
            continue
        for item in m.group(2).split(','):
            item = item.strip()
            if item:
                REEXPORT_MAP[(parent_mod, item)] = submod
    # Match: pub use [self::]submod::Item;
    for m in re.finditer(r'pub use (?:self::)?(\w+)::(\w+);', content):
        submod, item = m.group(1), m.group(2)
        if submod in ('std', 'rayon', 'tracing', 'windows', 'polars'):
            continue
        REEXPORT_MAP[(parent_mod, item)] = submod

parse_reexports(f"{ROOT}/index.rs", "index")
parse_reexports(f"{ROOT}/ntfs/mod.rs", "ntfs")
parse_reexports(f"{ROOT}/reader.rs", "reader")
parse_reexports(f"{ROOT}/commands/windows/mod.rs", "commands::windows")
parse_reexports(f"{ROOT}/io/readers/mod.rs", "io::readers")
parse_reexports(f"{ROOT}/io/readers/iocp/mod.rs", "io::readers::iocp")
parse_reexports(f"{ROOT}/io/parser/mod.rs", "io::parser")
parse_reexports(f"{ROOT}/platform.rs", "platform")
parse_reexports(f"{ROOT}/parse.rs", "parse")
parse_reexports(f"{ROOT}/io.rs", "io")
parse_reexports(f"{ROOT}/raw/mod.rs", "raw")
parse_reexports(f"{ROOT}/index/storage/mod.rs", "index::storage")
parse_reexports(f"{ROOT}/cache.rs", "cache")
parse_reexports(f"{ROOT}/usn.rs", "usn")

# Also parse cross-module re-exports (pub use crate::other_mod::Item)
def parse_cross_reexports(filepath, parent_mod):
    with open(filepath) as f:
        content = f.read()
    for m in re.finditer(r'pub use crate::(\w+(?:::\w+)*)::\{([^}]+)\};', content):
        target = m.group(1)
        for item in m.group(2).split(','):
            item = item.strip()
            if item:
                # This re-export in parent_mod points to crate::target::item
                # The caller uses parent_mod::item, needs target::item
                CROSS_REEXPORT_MAP[(parent_mod, item)] = target
    for m in re.finditer(r'pub use crate::(\w+(?:::\w+)*)::(\w+);', content):
        target, item = m.group(1), m.group(2)
        CROSS_REEXPORT_MAP[(parent_mod, item)] = target

CROSS_REEXPORT_MAP = {}
parse_cross_reexports(f"{ROOT}/io.rs", "io")
parse_cross_reexports(f"{ROOT}/io/fixup.rs", "io::fixup")
parse_cross_reexports(f"{ROOT}/io/merger.rs", "io::merger")
parse_cross_reexports(f"{ROOT}/io/parser/mod.rs", "io::parser")

print(f"Built map with {len(REEXPORT_MAP)} entries + {len(CROSS_REEXPORT_MAP)} cross-module")

# ─── Step 2: Delete all re-export lines ───────────────────────────────
REEXPORT_FILES = [
    f"{ROOT}/index.rs", f"{ROOT}/ntfs/mod.rs", f"{ROOT}/reader.rs",
    f"{ROOT}/commands/windows/mod.rs", f"{ROOT}/io/readers/mod.rs",
    f"{ROOT}/io/readers/iocp/mod.rs", f"{ROOT}/io/readers/parallel/mod.rs",
    f"{ROOT}/io/parser/mod.rs", f"{ROOT}/platform.rs", f"{ROOT}/parse.rs",
    f"{ROOT}/io.rs", f"{ROOT}/raw/mod.rs", f"{ROOT}/index/storage/mod.rs",
    f"{ROOT}/cache.rs", f"{ROOT}/io/fixup.rs", f"{ROOT}/io/merger.rs",
    f"{ROOT}/lib.rs", f"{ROOT}/usn.rs",
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
        # Skip pub use lines (single-line and multi-line)
        if stripped.startswith('pub use self::') or \
           stripped.startswith('pub use super::') or \
           stripped.startswith('pub use crate::') or \
           (stripped.startswith('pub use ') and '::' in stripped and
            not stripped.startswith('pub use std::') and
            not stripped.startswith('pub use rayon::') and
            not stripped.startswith('pub use tracing::') and
            not stripped.startswith('pub use windows::')):
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

# ─── Step 3: Rewrite callers ──────────────────────────────────────────
# For each .rs file in uffs-mft/src, replace import paths

# Build replacement pairs: ("crate::index::MftIndex", "crate::index::model::MftIndex")
REPLACEMENTS = []
for (parent, item), submod in sorted(REEXPORT_MAP.items(), key=lambda x: -len(x[0][1])):
    # Internal: crate::parent::item → crate::parent::submod::item
    old = f"crate::{parent}::{item}"
    new = f"crate::{parent}::{submod}::{item}"
    REPLACEMENTS.append((old, new))

# Also handle super:: references within submodules
# e.g. in index/model.rs: super::FileRecord → super::types::FileRecord
SUPER_REPLACEMENTS = {}
for (parent, item), submod in REEXPORT_MAP.items():
    if parent not in SUPER_REPLACEMENTS:
        SUPER_REPLACEMENTS[parent] = []
    SUPER_REPLACEMENTS[parent].append((item, submod))

def fix_grouped_uses(content, prefix, mapping):
    """Fix grouped use statements: use prefix::{A, B} where A→sub1, B→sub2."""
    pattern = re.compile(
        r'([ \t]*use ' + re.escape(prefix) + r'::\{)([^}]+)(\};)',
        re.DOTALL
    )
    def replacer(m):
        indent = m.group(1).split('use')[0]
        items_str = m.group(2)
        items = [i.strip() for i in items_str.split(',') if i.strip()]
        # Group items by their target submodule
        groups = {}  # submod → [items]
        unchanged = []
        for item in items:
            if item in mapping:
                sub = mapping[item]
                groups.setdefault(sub, []).append(item)
            else:
                unchanged.append(item)
        if not groups:
            return m.group(0)  # nothing to change
        lines = []
        if unchanged:
            lines.append(f'{indent}use {prefix}::{{{", ".join(unchanged)}}};')
        for sub in sorted(groups):
            grp = groups[sub]
            if len(grp) == 1:
                lines.append(f'{indent}use {prefix}::{sub}::{grp[0]};')
            else:
                lines.append(f'{indent}use {prefix}::{sub}::{{{", ".join(grp)}}};')
        return '\n'.join(lines)
    return pattern.sub(replacer, content)

def fix_file(fpath):
    with open(fpath) as f:
        content = f.read()
    original = content

    # Apply crate:: replacements for inline paths (longest item first)
    for old, new in REPLACEMENTS:
        content = content.replace(old, new)

    # Apply cross-module replacements
    for (parent, item), target in sorted(CROSS_REEXPORT_MAP.items(), key=lambda x: -len(x[0][1])):
        old_path = f"crate::{parent}::{item}"
        new_path = f"crate::{target}::{item}"
        content = content.replace(old_path, new_path)

    # Fix grouped use statements for each parent module
    for parent_mod in set(k[0] for k in REEXPORT_MAP):
        prefix = f"crate::{parent_mod}" if parent_mod else "crate"
        mapping = {item: sub for (p, item), sub in REEXPORT_MAP.items() if p == parent_mod}
        content = fix_grouped_uses(content, prefix, mapping)

    # Fix grouped use statements for cross-module re-exports
    for parent_mod in set(k[0] for k in CROSS_REEXPORT_MAP):
        prefix = f"crate::{parent_mod}"
        mapping = {}
        for (p, item), target in CROSS_REEXPORT_MAP.items():
            if p == parent_mod:
                # Need to rewrite crate::parent::item → crate::target::item
                # In grouped use, split by target module
                mapping[item] = target.replace(parent_mod + '::', '', 1) if target.startswith(parent_mod) else f"../{target}"
        # For cross-module, we can't just append submod — need full rewrite
        # Handle via inline replacements instead (already done above)

    # Apply super:: replacements based on which parent module this file is in
    rel = os.path.relpath(fpath, ROOT)
    parts = rel.replace('\\', '/').split('/')
    parent_ctx = None
    if len(parts) >= 2:
        if parts[0] == 'index' and len(parts) == 2:
            parent_ctx = 'index'
        elif parts[0] == 'ntfs' and len(parts) == 2:
            parent_ctx = 'ntfs'
        elif parts[0] == 'reader' and len(parts) == 2:
            parent_ctx = 'reader'
        elif parts[0] == 'io' and len(parts) == 2:
            parent_ctx = 'io'
        elif parts[0] == 'parse' and len(parts) == 2:
            parent_ctx = 'parse'
        elif parts[0] == 'platform' and len(parts) == 2:
            parent_ctx = 'platform'
        elif parts[0] == 'commands' and len(parts) >= 3 and parts[1] == 'windows':
            parent_ctx = 'commands::windows'
        elif parts[0] == 'io' and len(parts) >= 3:
            if parts[1] == 'readers':
                if len(parts) == 3:
                    parent_ctx = 'io::readers'
                elif len(parts) >= 4 and parts[2] == 'iocp':
                    parent_ctx = 'io::readers::iocp'
                elif len(parts) >= 4 and parts[2] == 'parallel':
                    parent_ctx = 'io::readers'
            elif parts[1] == 'parser':
                parent_ctx = 'io::parser'
        elif parts[0] == 'index' and len(parts) >= 3 and parts[1] == 'storage':
            parent_ctx = 'index::storage'
        elif parts[0] == 'raw' and len(parts) == 2:
            parent_ctx = 'raw'

    if parent_ctx and parent_ctx in SUPER_REPLACEMENTS:
        # Fix super:: grouped uses
        mapping = {item: sub for item, sub in SUPER_REPLACEMENTS[parent_ctx]}
        content = fix_grouped_uses(content, "super", mapping)
        # Fix standalone super:: references
        for item, submod in sorted(SUPER_REPLACEMENTS[parent_ctx], key=lambda x: -len(x[0])):
            old_s = f"super::{item}"
            new_s = f"super::{submod}::{item}"
            content = content.replace(old_s, new_s)

    if content != original:
        with open(fpath, 'w') as f:
            f.write(content)
        return True
    return False

changed = 0
for root_dir, dirs, files in os.walk(ROOT):
    for fname in files:
        if fname.endswith('.rs'):
            fpath = os.path.join(root_dir, fname)
            if fix_file(fpath):
                changed += 1
print(f"Updated {changed} files")
