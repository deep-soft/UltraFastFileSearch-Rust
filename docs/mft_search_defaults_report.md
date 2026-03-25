# Search defaults research for an NTFS/MFT browser

Date: 2026-03-25
Prepared for: product planning / default search preset design
Format: Markdown

---

## 1. Executive summary

I looked across the main desktop and CLI file-search ecosystems on Windows, macOS, Linux, and cross-platform tools. The most relevant tools are not all doing the same thing: some are instant filename/path searchers, some are full-text/document searchers, some are metadata-heavy query builders, and some are developer/admin command-line primitives. But once you normalize them by *user job*, the same patterns repeat.

The big result is simple:

1. The most common searches are still **name/path lookups**: "I know roughly what the file/folder is called; get me there now." This is the core workflow in Everything, WizFile, Spotlight, Listary, FSearch, plocate, fd, and GNU find. [R01][R02][R05][R06][R12][R19][R31][R35][R37][R38]
2. The next tier is **type/date/size filtering**: users narrow by extension, file kind, "modified today / last week", or "show me the big files". Those filters are first-class in Everything, WizFile, UltraSearch, SearchMyFiles, HoudahSpot, Spotlight, FSearch, and Agent Ransack. [R03][R05][R06][R08][R10][R17][R18][R20][R22][R23][R32]
3. The third tier is **content and regex search**: search inside docs, logs, emails, code, archives, and attachments. That is where Agent Ransack, AnyTXT, DocFetcher, Recoll, ripgrep, UltraSearch, Catfish, and Everything's slower `content:` mode matter. [R03][R08][R15][R16][R17][R18][R27][R29][R30][R33][R39]
4. Cleanup and power-user workflows keep recurring: **duplicates, hidden/system files, empty folders, long paths, NAS/network shares, and searching places the built-in index misses**. SearchMyFiles, Everything, Find Any File, EasyFind, UltraSearch, and WizFile all show strong evidence for those jobs. [R10][R24][R25][R26][R17][R18][R06][R51][R52]

For *your* product, the closest design references are:

- **Everything** for terse query grammar, bookmarks/history, and fast NTFS-first behavior. [R02][R03][R04]
- **WizFile** for MFT-based speed plus very approachable date/size operators in one box. [R05][R06]
- **SearchMyFiles** for cleanup workflows: duplicates, empty folders, time windows, folder scoping, hidden/system views. [R10]
- **Agent Ransack / FileLocator Pro** for content search, previews, Boolean text logic, and network-drive use. [R08][R09]
- **UltraSearch** for the "enterprise / network / SharePoint / ZIP / bulk actions" angle. [R17][R18]
- **Find Any File / EasyFind** for the "find what the index missed" mental model on macOS. [R24][R25][R26][R46]
- **fd + ripgrep + fzf** for developer and power-user workflows. [R38][R39][R40][R41][R42]

There is **no reliable public market-share dataset** for desktop file search. When I talk about popularity, I use public proxies instead: GitHub stars, longevity, third-party review counts, community references, and repeated appearance in comparison articles and forums. [R38][R39][R40][R48][R49][R42][R43]

My recommendation is to ship **20 presets grouped into four buckets**:

- Find by name / path
- Filter by type / date / size
- Search by content / metadata
- Cleanup / special cases

That gives you a default history which feels useful on day one for regular users, but still looks credible to IT, forensic, and developer users.

---

## 2. Methodology and caveats

### What I included

I looked at:

- Official docs, product pages, pricing pages, and manuals for Windows, macOS, Linux, and cross-platform search tools. [R01]-[R41]
- Community evidence from accessible public sources such as Hacker News, vendor forums, Listary forums, MacRumors, and practitioner blogs. [R42]-[R52]
- Public popularity signals such as GitHub stars for OSS CLI/search projects and Mac app review counts for long-running macOS utilities. [R38][R39][R40][R48][R49][R50]

### What I did *not* do

- I did **not** try to invent a fake market-share number. None of the vendors publish a credible cross-market installed-base dataset.
- I did **not** reduce all tools to Windows-only. Your product is NTFS/MFT-centric, but search habits are cross-platform and worth stealing from elsewhere.
- I did **not** assume that every tool has a stable one-line CLI syntax. Several important tools are primarily GUI query builders, so I represent them as saved-search recipes rather than fake shell commands.

### How I ranked the top 20 searches

This is a **qualitative weighted ranking**, not telemetry. I ranked search jobs higher when they showed up repeatedly across:

1. multiple platforms,
2. multiple tool categories,
3. official examples / built-in filters,
4. and community discussions about real workflows.

That means the top 20 below are best understood as **the most reusable preset ideas** rather than a claim like "exactly 13.7% of all users do this." The ranking is still very useful for product defaults because it captures what people keep trying to do regardless of UI style.

---

## 3. Tool landscape

## 3.1 Windows-first and NTFS-adjacent tools

### Everything

- Platform: Windows
- Search model: instant filename/path search; indexes local NTFS volumes on first run, then updates in real time. It can also include folders and file lists for non-NTFS or remote scenarios. [R01][R02]
- What people use it for most: fast partial-name lookup, type/date/size filters, recent-file monitoring, sorting by run count / modified time, bookmarks, and increasingly duplicate/content edge cases. [R02][R03][R42][R51][R52]
- Cost: free download / freeware style distribution. [R01][R02]
- Target audience: Windows power users, IT/admins, developers, data hoarders, and anyone replacing Windows Search. [R01][R02][R42]
- Why it matters to your product: it is the most important Windows mental model to study for a terse but powerful query grammar. [R02][R03][R04]

### WizFile

- Platform: Windows
- Search model: very fast MFT-based search on NTFS volumes; query box supports wildcards, date/size operators, folder/file-only switches, and regex. [R05][R06]
- What people use it for most: name/path lookup, big-file hunting, recent-file hunting, path-length cleanup, and general "find it instantly" work. [R05][R06]
- Cost: free for personal use; supporter code / enterprise licensing for broader or commercial use. [R05][R07]
- Target audience: mainstream Windows users and power users who want speed with very low UI friction. [R05][R06]
- Why it matters to your product: it proves that size/date operators can be made very approachable without a heavy advanced-search dialog. [R05][R06]

### UltraSearch

- Platform: Windows
- Search model: enterprise-leaning file and content search across local storage, network storage, and SharePoint; supports filters, previews, regex, and bulk file actions. [R17][R18]
- What people use it for most: filename lookup at scale, content lookup, filtering by age/size/type, and searching across network locations or SharePoint. [R17][R18]
- Cost: free edition for private use; paid professional tier starts from $2.50 per user per month, with a 30-day full-feature period. [R17]
- Target audience: teams, IT, power users, and enterprise environments. [R17]
- Why it matters to your product: it is one of the cleanest examples of taking "Everything-like speed" into team and network scenarios. [R17][R18]

### Agent Ransack / FileLocator Pro

- Platform: Windows
- Search model: filename + content search with preview, text highlighting, Boolean logic, regex, date filters, and network-drive support. [R08][R09]
- What people use it for most: content search inside logs/docs/source, network-drive search, and more forensic / audit-style lookups than Everything or WizFile. [R08]
- Cost: Lite / free mode for personal and commercial use; Pro license works with Agent Ransack and FileLocator Pro, with standard pricing at $79 and one year of major upgrades. [R08][R09]
- Target audience: IT, support, compliance, admins, and users who search inside files more than they search by filename. [R08][R09]
- Why it matters to your product: the preview-and-highlight workflow is stronger than most filename-first tools. [R08]

### SearchMyFiles

- Platform: Windows
- Search model: accurate filesystem scan with strong filters for wildcard, time, attributes, size, content, duplicates, folder scoping, summary mode, and empty-folder search. [R10]
- What people use it for most: precise time-window searches, duplicate finding, cleanup, hidden/system file inspection, empty folder removal, and scoped searches inside specific folder patterns. [R10]
- Cost: freeware / NirSoft utility. [R11]
- Target audience: Windows power users, admins, incident-response / forensic users, and cleanup-oriented users. [R10][R11]
- Why it matters to your product: if your product wants to feel more useful than a pure MFT browser, SearchMyFiles is the best blueprint for cleanup presets. [R10]

### Listary

- Platform: Windows
- Search model: fast file search plus app launcher; fuzzy matching; filters like `folder:`, `doc:`, `pic:`, `video:`, `audio:`; strong Explorer integration. [R12][R13]
- What people use it for most: quick filename lookup from Explorer context, current-folder search, recents, file-type filtered search, and app launching. [R12][R13][R44][R45]
- Cost: free for personal use; Pro is a one-time $19.95 and adds shared drive indexing, advanced syntax, and commercial rights. [R14]
- Target audience: regular Windows users, knowledge workers, and power users who live in Explorer. [R12][R14]
- Why it matters to your product: it is a strong example of "search where I am now" and recency-aware ranking. [R44][R45]

### AnyTXT Searcher

- Platform: Windows
- Search model: indexed full-text search for documents and many formats; positioned as a "local Google" for file content. [R15][R16]
- What people use it for most: searching inside PDFs, Office docs, ebooks, images/OCR, and other document repositories by phrase or keyword. [R15][R16]
- Cost: free desktop search tool. [R15][R16]
- Target audience: document-heavy Windows users, researchers, students, and office users. [R15]
- Why it matters to your product: it shows what users expect once they graduate from filename search to content search. [R15][R16]

## 3.2 macOS search ecosystem

### Spotlight / mdfind

- Platform: macOS (built-in)
- Search model: system index with keyword/type filters and metadata query syntax; GUI in Spotlight and CLI via `mdfind`. [R19][R20][R21]
- What people use it for most: fast general lookup, app/file launch, kind-based filtering, and metadata search by fields like author, title, tag, and kind. [R19][R20][R21]
- Cost: built into macOS. [R19][R20]
- Target audience: all Mac users by default. [R19]
- Why it matters to your product: it sets the expectation for metadata-aware search terms and lightweight CLI access. [R20][R21]

### HoudahSpot

- Platform: macOS
- Search model: advanced query builder on top of Spotlight, with templates, multi-criteria searches, previews, and automation. [R22][R23]
- What people use it for most: precise multi-field searches such as invoices, client documents, image dimensions, recent files, and saved recurring searches. [R22][R23][R43]
- Cost: $34 single-user license; upgrades from previous versions are available at $19. [R23]
- Target audience: professionals and power users managing large file collections. [R22][R23]
- Why it matters to your product: saved-search templates are a very strong idea if you want more than a simple history list. [R22][R23]

### Find Any File (FAF)

- Platform: macOS
- Search model: searches directly on disks instead of depending on Spotlight; finds hidden items, package contents, NAS/external volumes, and excluded folders. [R24][R25]
- What people use it for most: "Spotlight missed it", NAS search, hidden/system/package search, and saved searches for problem cases. [R24][R25][R43][R46]
- Cost: $6 direct purchase, $8 in the App Store. [R25]
- Target audience: Mac power users, tinkerers, admins, NAS users, and people searching outside Spotlight's comfort zone. [R24][R25]
- Why it matters to your product: this is the clearest macOS example of the "find the missing / hidden / excluded thing" workflow. [R24][R25]

### EasyFind

- Platform: macOS
- Search model: no-index search by name, content, tags, comments, Boolean operators, wildcards, phrases, and regex. [R26]
- What people use it for most: direct filesystem search when Spotlight is incomplete or unreliable; hidden-file and Finder-miss cases come up repeatedly in user discussions. [R26][R46]
- Cost: freeware. [R26]
- Target audience: Mac users who want a lightweight Spotlight alternative. [R26][R46]
- Why it matters to your product: it shows there is durable demand for "search without trusting the index". [R26][R46]

## 3.3 Linux / Unix desktop search ecosystem

### FSearch

- Platform: Linux / Unix-like desktops
- Search model: Everything-inspired fast filename search utility with advanced search syntax, custom filters, and instant results. [R31][R32]
- What people use it for most: Everything-like filename/path search, extension/date/size filtering, and saved filters for common locations such as backup drives. [R31][R32]
- Cost: open source / free. [R31]
- Target audience: advanced Linux desktop users who want Everything-style speed. [R31]
- Why it matters to your product: it is the strongest Linux analog to Everything in both spirit and syntax style. [R31][R32]

### Catfish

- Platform: Linux / Unix (Xfce ecosystem, but broader availability)
- Search model: versatile GUI file search with date/type filters and full-text search support via locate / Zeitgeist. [R33]
- What people use it for most: general desktop searches with lightweight UI, filtering by date/type, and simple content lookup. [R33]
- Cost: open source / free. [R33]
- Target audience: regular Linux desktop users. [R33]

### GNOME LocalSearch / Tracker

- Platform: GNOME / Linux
- Search model: system indexer and search engine powering GNOME desktop search; exposes D-Bus services and command-line utilities. [R34]
- What people use it for most: system-wide desktop search and metadata extraction through GNOME components. [R34]
- Cost: built-in / open source. [R34]
- Target audience: GNOME users and developers. [R34]

### plocate

- Platform: Linux / Unix-like systems
- Search model: indexed filename search, locate-compatible but faster; supports substring, glob, basename, count, and regex modes. [R35][R36]
- What people use it for most: instant filename lookup, shell scripts, counts, and administrative searches where a prebuilt index is acceptable. [R35][R36]
- Cost: open source / free. [R35]
- Target audience: admins, shell users, and people who prefer indexed name search in the terminal. [R35][R36]

### GNU find

- Platform: POSIX / Linux / Unix / macOS environments
- Search model: live recursive filesystem traversal with tests for name, type, time, size, owner, permissions, contents, and actions. [R37]
- What people use it for most: precise scripted audits, cleanup, recursive searches, permission-oriented tasks, and "do something with the result" pipelines. [R37]
- Cost: free / standard toolchain component. [R37]
- Target audience: admins, developers, forensics, automation. [R37]

## 3.4 Cross-platform document/content search tools

### DocFetcher

- Platform: Windows, Linux, macOS
- Search model: open-source desktop search for file contents; portable versions; filters for file type, location, and size. [R27]
- What people use it for most: local document repositories, content search in PDFs/docs/spreadsheets, and a Google-like local search experience. [R27]
- Cost: open source / free basic version; paid Pro and Server products exist separately. [R27][R28]
- Target audience: office users, researchers, students, knowledge workers. [R27]

### Recoll

- Platform: Unix, Linux, Windows, macOS
- Search model: full-text desktop/document search built on Xapian; deep support for archives, emails, metadata fields, and CLI querying. [R29][R30]
- What people use it for most: content-heavy search across document collections, email archives, ZIPs, embedded attachments, and metadata-rich corpora. [R29][R30][R47]
- Cost: open source / free. [R29]
- Target audience: researchers, archivists, knowledge workers, Linux power users, and anyone searching large heterogeneous document corpora. [R29][R30][R47]

## 3.5 CLI and developer-oriented search stack

### fd

- Platform: Windows, macOS, Linux
- Search model: fast user-friendly `find` alternative with regex/glob patterns, hidden/ignore controls, file-type filters, change-time filters, and good ergonomics. [R38]
- What people use it for most: searching code trees by name/extension, finding folders like `node_modules`, hidden files such as `.env`, and feeding file lists into editors or fzf. [R38][R40]
- Cost: open source / free. [R38]
- Popularity signal: about 42.2k GitHub stars at crawl time. [R38]

### ripgrep (rg)

- Platform: Windows, macOS, Linux
- Search model: recursive regex/content search that respects ignore files by default and supports file-type filters, compressed files, and PCRE2. [R39]
- What people use it for most: code search, log search, content search in large trees, and pairing with filename search tools such as Everything or fzf. [R39][R42]
- Cost: open source / free. [R39]
- Popularity signal: about 61.4k GitHub stars at crawl time. [R39]

### fzf

- Platform: Windows, macOS, Linux
- Search model: fuzzy selector for arbitrary lists; commonly paired with fd or ripgrep for file and content workflows. [R40][R41]
- What people use it for most: interactive file picking, history/process/bookmark search, and live ripgrep-powered content search. [R40][R41]
- Cost: open source / free. [R40]
- Popularity signal: about 79k GitHub stars at crawl time. [R40]

---

## 4. Popularity and target-audience signals

Because there is no clean market-share report, the best way to think about the market is by **segment dominance** rather than one universal winner.

### Likely segment leaders by mindshare

- **Windows instant filename/path search**: Everything is the reference implementation in community discussion, while WizFile is the closest mainstream alternative and UltraSearch is the enterprise/network-heavy cousin. [R01][R02][R05][R17][R42]
- **Windows content search**: Agent Ransack/FileLocator Pro, AnyTXT, and DocFetcher are the clearest "search inside files" options. [R08][R15][R27]
- **macOS advanced search**: Spotlight is the default baseline, HoudahSpot is the most explicit saved-search/query-builder upgrade, and Find Any File / EasyFind own the "search beyond Spotlight" niche. [R19][R22][R24][R26][R43][R46]
- **Linux desktop filename search**: FSearch is the most Everything-like desktop app; Catfish and GNOME LocalSearch are broader desktop-oriented options. [R31][R33][R34]
- **Cross-platform developer workflows**: fd + ripgrep + fzf is the most visible stack by OSS popularity and community usage. [R38][R39][R40][R41]

### Public popularity proxies

- `fd`: about 42.2k GitHub stars. [R38]
- `ripgrep`: about 61.4k GitHub stars. [R39]
- `fzf`: about 79k GitHub stars. [R40]
- `FSearch`: about 4.1k GitHub stars, which is much smaller than the CLI tools but large for a desktop Linux search utility. [R50]
- `Find Any File`: MacUpdate page showed 176 user reviews at crawl time. [R48]
- `EasyFind`: MacUpdate page showed 194 user reviews at crawl time. [R49]

These are not market share. They are just useful hints about public visibility and long-term adoption.

### Audience map

- **Regular desktop users**: WizFile, Listary, Spotlight, Catfish, EasyFind. [R05][R12][R19][R26][R33]
- **Power users / IT / support**: Everything, SearchMyFiles, Agent Ransack, UltraSearch, Find Any File. [R02][R08][R10][R17][R24]
- **Developers**: fd, ripgrep, fzf, GNU find, Everything + ripgrep on Windows. [R38][R39][R40][R41][R42]
- **Knowledge workers / researchers**: DocFetcher, Recoll, AnyTXT, HoudahSpot. [R15][R22][R27][R29]
- **Enterprise / team search**: UltraSearch Professional / DataCentral and FileLocator Pro. [R09][R17][R18]

---

## 5. What users actually use these tools for most of the time

Across docs and community evidence, the recurring jobs are surprisingly stable.

### A. Known-item retrieval

This is the most universal workflow: "I know part of the file or folder name." Everything, WizFile, Spotlight, Listary, FSearch, plocate, fd, and find all make this the front-door use case. [R02][R05][R06][R19][R31][R35][R37][R38]

### B. Filter by type

Users very often start with a file type: PDFs, Office docs, images, music, videos, source files, logs, installers. Many tools expose built-in filters or extensions for this because it is one of the most common next refinements after name search. [R03][R05][R06][R13][R17][R20][R32][R39]

### C. Filter by date / recency

"Modified today", "opened in the last week", "created recently", and "show me the recent files in the current folder" are everywhere. Everything, WizFile, SearchMyFiles, Listary, Spotlight, HoudahSpot, FSearch, and Agent Ransack all expose date or recency workflows. [R02][R06][R08][R10][R20][R22][R32][R45]

### D. Filter by size

Big-file hunting is one of the most persistent jobs because users often use search tools as a lightweight storage-management tool. WizFile, Everything, SearchMyFiles, UltraSearch, and FSearch all foreground size filters. [R03][R06][R10][R17][R18][R32]

### E. Search inside files

As soon as users stop remembering filenames, they switch to searching for the text inside docs, logs, emails, code, or notes. This is the reason Agent Ransack, AnyTXT, DocFetcher, Recoll, ripgrep, and UltraSearch all exist and continue to matter. [R08][R15][R17][R27][R29][R39][R47]

### F. Scope to the right place

Users repeatedly want "current folder only", "this client/project tree", "only these subfolders", "only backup drive", or "only NAS/network/SharePoint". That pattern appears in Listary forum posts, SearchMyFiles folder filters, Spotlight `-onlyin`, FSearch custom filters, and UltraSearch location targeting. [R10][R18][R21][R32][R44]

### G. Search what the index missed

This shows up strongly on macOS and in forensic/admin workflows. Find Any File and EasyFind are explicitly about finding files Spotlight does not; Everything and SearchMyFiles also expose hidden/system/attribute and non-default locations. [R10][R24][R26][R46]

### H. Cleanup / duplicate / audit tasks

A large number of public examples are really "search as maintenance": duplicates, empty folders, large files, long paths, old files, and stale content. SearchMyFiles and Everything are especially strong here; WizFile, UltraSearch, and Find Any File also point in this direction. [R06][R10][R17][R25][R51][R52]

### I. Developer and support workflows

Developers and support users repeatedly pair filename search with content search: fd or Everything for locating files, ripgrep for text/regex inside them, and fzf for interactive narrowing. Community evidence explicitly mentions pairing Everything with ripgrep on Windows, and fd/fzf or ripgrep/fzf on Unix-like systems. [R38][R39][R40][R41][R42]

### J. Metadata-heavy professional search

HoudahSpot, Spotlight, Recoll, and Everything's metadata functions show another durable pattern: users often want author, title, tag, image dimensions, or document-specific fields rather than only name/content. [R03][R20][R21][R22][R23][R30]

---

## 6. Ranked top 20 search presets to preconfigure

Below is the ranked list I would actually use to seed the default history/preset library for an NTFS/MFT browser.

### 1. Find a file by partial name

Why it ranks first: this is the universal default behavior of nearly every search tool in the set. [R02][R05][R19][R31][R35][R38]

### 2. Find a folder by name

Why it ranks second: many users are really trying to reach a folder (`node_modules`, `Photos`, `Invoices`, `Backups`, client directories) rather than a specific file. Folder-only filters are first-class in Everything, WizFile, Listary, Spotlight, fd, and find. [R03][R06][R13][R20][R38][R37]

### 3. Find all files of a given type / extension

Why it ranks high: extension and type filters are everywhere and are often the first refinement after a name guess. [R03][R06][R13][R17][R20][R32][R39]

### 4. Find files modified recently (today / last 7 days / this month)

Why it ranks high: recency is one of the strongest user-memory cues. [R02][R06][R10][R20][R22][R32][R45]

### 5. Find files opened / run recently

Why it ranks high: users often remember interaction time better than modification time; Everything and Listary expose this explicitly, and macOS users often think in terms of recently used files too. [R02][R45][R43]

### 6. Find large files

Why it ranks high: users use search tools to recover space and to understand disk usage without opening a separate storage analyzer. [R03][R06][R10][R17][R18][R32]

### 7. Search inside documents for a word or phrase

Why it ranks high: as soon as the filename is forgotten, content search becomes the next default strategy. [R08][R15][R17][R27][R29][R47]

### 8. Search logs or code with regex / text patterns

Why it ranks high: this is the dominant power-user / developer / support workflow. It also generalizes beyond code to CSV exports, structured file names, and operational logs. [R04][R06][R08][R18][R39][R41][R42]

### 9. Search only inside a specific folder tree or project path

Why it ranks high: users frequently know *where* but not *what*; scoping reduces noise dramatically. [R10][R21][R32][R44]

### 10. Find hidden, system, or otherwise excluded files

Why it ranks high: these are common in troubleshooting, uninstall cleanup, forensics, and "why can't the built-in search find this" moments. [R10][R24][R26][R46]

### 11. Find duplicate files

Why it ranks high: duplicate hunting repeatedly appears in vendor docs and forums because it is a high-value cleanup task. [R10][R25][R51][R52]

### 12. Find duplicate names (with or without identical content)

Why it ranks high: users often need to reconcile confusing duplicate names even when the files are not byte-identical. SearchMyFiles and Everything both support strong variants of this. [R10][R51][R52]

### 13. Find executables, installers, scripts, or app files

Why it ranks high: this is common in admin, malware-hunting, troubleshooting, and general "where is that program" use. Spotlight has `kind:app`; fd and find expose executable/type filters; Everything and WizFile make extension-driven searches easy. [R05][R06][R20][R37][R38]

### 14. Find project config / manifest / dotfiles

Why it ranks high: this is especially common among developers and ops users (`README`, `.env`, `package.json`, `Cargo.toml`, `.toml`, `.ini`, etc.). fd, ripgrep, Everything, and FSearch all make this ergonomic. [R03][R32][R38][R39][R40]

### 15. Find documents by metadata such as author, tag, or title

Why it ranks high: professionals often remember *who* or *what the document was about* better than the exact filename. [R03][R20][R21][R22][R23][R30]

### 16. Find photos, videos, or audio by media-specific fields

Why it ranks high: image dimensions, orientation, tags, track metadata, and file groups are explicit features in several tools. [R03][R13][R18][R22][R23][R32]

### 17. Search inside archives, ZIPs, attachments, or compressed files

Why it ranks high: this is a strong differentiator in advanced tools and a real user need in research, legal, support, and archive-heavy environments. [R17][R24][R29][R30][R39][R47]

### 18. Find empty folders or stale folder structures

Why it ranks high: cleanup/search tools get used for pruning and folder audits, not only finding files. SearchMyFiles has an explicit summary/empty-folder workflow. [R10]

### 19. Search NAS, network shares, external drives, or snapshots

Why it ranks high: a lot of modern file retrieval happens outside the local default index. Find Any File, UltraSearch, Listary Pro, and Everything file lists all point to this. [R02][R14][R17][R24][R43]

### 20. Find long paths or problematic names

Why it ranks high: it is not universal, but it is very useful in Windows-heavy and cleanup-heavy environments, and dedicated tools expose it explicitly. [R06][R10]

### Practical reading of the ranking

If you only ship 8 presets, use items 1-8.
If you ship 12, use items 1-12.
If you want a strong "power user" first-run experience, ship all 20 but group them visibly.

---

## 7. Recommended preset buckets for your product

If you present the defaults as one flat list, they will feel messy. A better shape is:

### Quick Find

1. Find file by name
2. Find folder by name
3. Find by type / extension
4. Find recent files
5. Find recent folders / recently opened files

### Triage and Cleanup

6. Find large files
7. Find duplicates
8. Find duplicate names
9. Find empty folders
10. Find long paths / problematic names

### Power Search

11. Search inside documents
12. Search logs / code with regex
13. Search within a specific path tree
14. Find hidden/system files
15. Search archives / compressed content

### Professional / Metadata

16. Find by author / tag / title
17. Find media by dimensions / media fields
18. Find executables / installers / scripts
19. Find project configs / manifests / dotfiles
20. Search NAS / network / external locations

That grouping mirrors how users mentally escalate:

- first by name,
- then by narrowing filters,
- then by content,
- then by cleanup or special cases.

---

## 8. Native-syntax cookbook

The examples below are **new example searches** built from each tool's documented syntax. They are not a literal copy of any vendor preset list.

## 8.1 Everything query box / ES syntax [R02][R03][R04]

```text
# Type these into the Everything search box unless you prefer ES / es.exe.

# Find all Rust source files
*.rs

# Find all executable files by extension
*.exe

# Find folders named node_modules
folder: node_modules

# Find files modified today
dm:today

# Find files opened today
dr:today

# Find files between 500 MB and 1 GB
size:500mb..1gb

# Find recent emails containing a word in the file content
*.eml dm:thisweek content:banana

# Find log-like file names with regex
regex:\.log$

# Find images wider than 2560 pixels
width:>2560

# Find MP3s from a year range
year:2002..2005
```

## 8.2 WizFile search-box syntax [R05][R06][R07]

```text
# Type these directly into the WizFile search box.

# Find all MP3 and WAV files
*.mp3|*.wav

# Find folders named node_modules
node_modules =folder

# Find files modified in the last 7 days
>=today-7

# Find files larger than 1 GB modified in the last 30 days
>=1gb >=today-30

# Find files containing music but exclude MP3 files
music !*.mp3

# Find files between 500 MB and 1 GB
>=500mb <=1gb

# Find paths longer than 200 characters
pathlen>200

# Regex search for date-stamped CSV exports
/[0-9]{4}-[0-9]{2}-[0-9]{2}\.csv$

# Search files only, not folders
error =file
```

## 8.3 FSearch query syntax [R31][R32]

```text
# Find every MP4 file larger than 1 GB
ext:mp4 size:>1gb

# Find JPG and PNG files modified last month with family names in the filename
(mum OR dad) ext:jpg;png dm:lastmonth

# Search a custom backup filter for files older than 3 years
backup: dm:<past3years

# Find all Rust source files
ext:rs

# Find all TOML files
ext:toml

# Find log files by extension
ext:log

# Find PNGs but exclude JPEGs
ext:png NOT ext:jpg
```

## 8.4 Spotlight / mdfind (macOS CLI) [R19][R20][R21]

```bash
# Find folders named node_modules under Projects only
mdfind -onlyin ~/Projects 'kind:folder node_modules'

# Find PDF files related to invoices
mdfind 'kind:pdf invoice'

# Find image files named logo
mdfind 'kind:image logo'

# Find documents by author
mdfind 'author:John'

# Find documents by title keyword
mdfind 'title:"Q4 budget"'

# Advanced metadata query: files authored by Steve
mdfind 'kMDItemAuthors ==[c] "Steve"'
```

## 8.5 Listary search-box filters [R12][R13][R44][R45]

```text
# Type these in Listary's launcher / search UI.

# Find folders with Listary in the name
folder: Listary

# Find documents related to invoices
doc: invoice

# Find pictures with logo in the name
pic: logo

# Find videos related to trailers
video: trailer

# Find audio files related to podcasts
audio: podcast

# Search recent files/folders globally
> report
```

## 8.6 fd (cross-platform CLI) [R38]

```bash
# Find all Rust source files
fd --type file --extension rs

# Find directories named node_modules
fd --type directory node_modules

# Find the exact file name Cargo.toml
fd --glob 'Cargo.toml'

# Include hidden and ignored files to find .env files anywhere
fd -HI '.env'

# Find files changed within the last 7 days
fd --changed-within 7d

# Match a full path pattern such as .git/config
fd -p -g '**/.git/config'

# Search for README; uppercase makes smart-case behave case-sensitively
fd README
```

## 8.7 ripgrep (content / regex search) [R39][R41][R42]

```bash
# Search Python files for a symbol
rg -tpy 'async def'

# Search everything, including hidden and ignored files, for ERROR
rg -uuu 'ERROR'

# Search for a constant as a whole word and show line numbers
rg -n -w '[A-Z]+_SUSPEND'

# Exclude JavaScript files from the search
rg -Tjs 'password'

# Search compressed files too
rg -z 'panic|exception'

# Interactive ripgrep inside fzf
fzf --disabled --bind 'change:reload:rg {q}'
```

## 8.8 GNU find [R37]

```bash
# Find all Rust source files
find . -type f -name '*.rs'

# Find directories named node_modules
find . -type d -name 'node_modules'

# Find files larger than 1 GB
find . -type f -size +1G

# Find files modified in the last 7 days
find . -type f -mtime -7

# Find README files case-insensitively
find . -type f -iname 'README'

# Find empty directories
find . -type d -empty

# Example from real macOS cleanup discussion: search remnant Google files in ~/Library
find ~/Library -iname '*google*'
```

## 8.9 plocate [R35][R36]

```bash
# Fast indexed lookup for anything containing node_modules
plocate node_modules

# Match only against the basename, not the whole path
plocate -b Cargo.toml

# Find log files by glob pattern
plocate -b '*.log'

# Regex match for TOML files
plocate --regex '.*\.toml$'

# Count matches instead of printing them
plocate -c node_modules
```

## 8.10 Recoll / recollq [R29][R30]

```bash
# File-name oriented query
recollq -f report

# Search by author / text / boolean logic
recollq 'author:"john doe" Beatles OR Lennon -potatoes'

# Search messages category for invoice
recollq 'rclcat:message invoice'

# Search by filename field and author field together
recollq 'filename:report author:alice'

# General content query
recollq 'invoice AND april'
```

---

## 9. GUI-native saved-search recipes for tools without a clean one-line syntax

These are useful because several important products are query builders first, shell tools second.

### SearchMyFiles recipes [R10]

**Recipe: last-minute change window**
- Base folder: `C:\`
- Files wildcard: `*`
- File time: modified in the last 10 minutes
- Size: 500-700 bytes
- Search subfolders: on

**Recipe: scoped image search**
- Base folder: `C:\Shared`
- Include Only Folders: `C:\Shared\*\Images`
- Files wildcard: `*.jpg;*.png`

**Recipe: duplicate cleanup**
- Search mode: `Duplicates Search`
- Optional narrowing: only files larger than 500 KB

**Recipe: empty folder cleanup**
- Search mode: `Summary Mode`
- Summary filter: `Only folders with zero files and subfolders`

### Agent Ransack / FileLocator Pro recipes [R08][R09]

**Recipe: recent log triage**
- File name: `*.log`
- Containing text: `ERROR OR WARN`
- Date modified: last 7 days
- Scope: local folder or network drive

**Recipe: document content search**
- File name: `*.docx;*.pdf;*.txt`
- Containing text: customer / incident phrase
- Enable text preview and highlight

### UltraSearch recipes [R17][R18]

**Recipe: enterprise content search**
- Search target: local drive + network share + SharePoint
- File type: Office / PDFs
- Content text: invoice or client keyword
- Optional: search inside ZIP (Pro)

**Recipe: bulky stale media**
- File type: Pictures / Video
- Size: over 1 GB
- Age: older than 30 days

### HoudahSpot recipes [R22][R23][R43]

**Recipe: client files from the last week**
- Kind: document
- Text contains: client name
- Last modified / opened: last 7 days

**Recipe: logo asset hunt**
- Kind: image
- Name contains: `logo`
- Width: 512 px

### Find Any File / EasyFind recipes [R24][R25][R26][R46]

**Recipe: hidden remnant files after app uninstall**
- Search path: `~/Library`
- Include hidden files / package contents
- Name contains: app / vendor keyword

**Recipe: NAS-only archive search**
- Search path: NAS mount point or external volume
- Search by name or content depending on tool
- Save the search if it is a recurring archive workflow

---

## 10. What I would actually preload in version 1

If I had to choose one concrete default bundle for an NTFS/MFT browser, it would be this 20-item set, in this order:

1. Find file by name
2. Find folder by name
3. Find documents (`*.pdf`, `*.docx`, `*.xlsx`, `*.pptx`)
4. Find images (`*.jpg`, `*.png`, `*.gif`, `*.webp`)
5. Find videos (`*.mp4`, `*.mkv`, `*.mov`)
6. Find executables / installers (`*.exe`, `*.msi`, `*.bat`, `*.cmd`)
7. Find recent files modified today
8. Find files modified in the last 7 days
9. Find recently opened / run files
10. Find large files
11. Search inside documents for text
12. Search logs for regex / error text
13. Search within a specific project / client tree
14. Find hidden / system files
15. Find duplicate files
16. Find duplicate names
17. Find project manifests / config files (`package.json`, `Cargo.toml`, `.toml`, `.env`, `.ini`, `.json`, `.yaml`)
18. Find media by metadata / dimensions
19. Find empty folders
20. Search network / external / file-list locations

### Why this exact bundle

- Items 1-10 cover the highest-frequency desktop needs.
- Items 11-17 make the product feel serious to power users.
- Items 18-20 prevent the history from feeling like it only serves developers.

---

## 11. Product-design implications for your app

A few design lessons stood out from the research.

### A. Keep the first box simple, but let it grow

The best tools all start with one obvious search box, then allow operators or filters to accumulate without forcing users into a modal advanced-search form too early. Everything, WizFile, Spotlight, and FSearch all benefit from this. [R02][R05][R20][R31]

### B. Separate cheap vs expensive searches

Users do not always understand the performance difference between:

- name/path lookup from an index or MFT, and
- content/duplicate/hash searches.

If your product exposes both, mark some presets as "fast" and others as "deeper / slower". Everything explicitly warns that content search is slow and should be combined with other filters. [R03]

### C. Scope controls matter almost as much as the query itself

Current folder, project folder, drive, network share, and saved-location filters are a major part of real usage. [R10][R17][R21][R32][R44]

### D. Presets are better than history alone

History is useful, but tools like HoudahSpot and FSearch show that named templates / filters are stronger for recurring workflows. [R22][R23][R32]

### E. Cleanup workflows are underrated

Duplicate files, empty folders, long paths, hidden files, and big files may not be "search" in the purest sense, but users repeatedly use search tools for exactly that. SearchMyFiles is the strongest proof. [R10]

---

## 12. Bottom line

If your goal is to preconfigure the top searches people actually do in file-search tools, the winning strategy is **not** to mirror one tool. It is to blend:

- Everything/WizFile for Windows speed and terse syntax,
- SearchMyFiles for cleanup and forensic-style presets,
- Agent Ransack / AnyTXT / DocFetcher / Recoll for content search ideas,
- Spotlight / HoudahSpot / Find Any File for metadata and out-of-index behavior,
- and fd / ripgrep / fzf for power-user credibility.

If you preload the 20 searches listed in Section 10, grouped by bucket, your product will feel immediately useful to ordinary desktop users while still speaking the language of developers, IT, and advanced users.

---

## References

[R01] voidtools, "Everything" - https://www.voidtools.com/support/everything/
[R02] voidtools, "Using Everything" - https://www.voidtools.com/support/everything/using_everything/
[R03] voidtools, "Searching" - https://www.voidtools.com/support/everything/searching/
[R04] voidtools, "Command Line Interface" - https://www.voidtools.com/support/everything/command_line_interface/
[R05] Antibody Software, "WizFile" - https://antibody-software.com/wizfile/
[R06] Antibody Software, "WizFile Quick Start Guide" - https://antibody-software.com/wizfile/quick-start
[R07] Antibody Software, "WizFile FAQ" - https://antibody-software.com/wizfile/faq
[R08] Mythicsoft, "Agent Ransack" - https://www.mythicsoft.com/agentransack/
[R09] Mythicsoft, "Pro License Purchase" - https://www.mythicsoft.com/pro-license-purchase/
[R10] NirSoft, "SearchMyFiles" - https://www.nirsoft.net/utils/search_my_files.html
[R11] NirSoft, homepage / freeware utilities - https://www.nirsoft.net/
[R12] Listary Docs, "Search Files" - https://help.listary.com/search-file
[R13] Listary Docs, "Filters" - https://help.listary.com/options-filters
[R14] Listary Pro pricing - https://www.listary.com/pro
[R15] AnyTXT Searcher overview - https://anytxt.net/
[R16] AnyTXT Searcher download / changelog - https://anytxt.net/download/
[R17] JAM Software, "UltraSearch" - https://www.jam-software.com/ultrasearch
[R18] JAM Software, "UltraSearch Guided Tour" - https://www.jam-software.com/ultrasearch/guided-tour.shtml
[R19] Apple Support, "Search for anything with Spotlight on Mac" - https://support.apple.com/guide/mac-help/search-with-spotlight-mchlp1008/mac
[R20] Apple Support, "Narrow your search results in Spotlight on Mac" - https://support.apple.com/guide/mac-help/narrow-search-results-in-spotlight-mchl4d69efd3/mac
[R21] Apple Developer Archive, "File Metadata Query Expression Syntax" - https://developer.apple.com/library/archive/documentation/Carbon/Conceptual/SpotlightQuery/Concepts/QueryFormat.html
[R22] HoudahSpot home - https://www.houdah.com/houdahSpot/
[R23] HoudahSpot features / pricing - https://www.houdahspot.com/powerful-mac-file-search.html
[R24] Find Any File home - https://findanyfile.app/
[R25] Find Any File purchase - https://findanyfile.app/purchase.html
[R26] DEVONtechnologies freeware / EasyFind - https://www.devontechnologies.com/apps/freeware
[R27] DocFetcher - https://docfetcher.sourceforge.io/
[R28] DocFetcher Pro - https://docfetcherpro.com/
[R29] Recoll home - https://www.recoll.org/index.html
[R30] Recoll user manual - https://www.recoll.org/usermanual/usermanual.html
[R31] FSearch home - https://cboxdoerfer.github.io/fsearch/
[R32] FSearch feature release / syntax examples - https://github.com/cboxdoerfer/fsearch/discussions/381
[R33] Catfish docs - https://docs.xfce.org/apps/catfish/introduction
[R34] GNOME LocalSearch overview - https://gnome.pages.gitlab.gnome.org/localsearch/overview.html
[R35] plocate home - https://plocate.sesse.net/
[R36] plocate man page - https://plocate.sesse.net/plocate.1.html
[R37] GNU Findutils manual - https://www.gnu.org/software/findutils/manual/html_mono/find.html
[R38] fd GitHub - https://github.com/sharkdp/fd
[R39] ripgrep GitHub - https://github.com/BurntSushi/ripgrep
[R40] fzf GitHub - https://github.com/junegunn/fzf
[R41] fzf ripgrep integration - https://junegunn.github.io/fzf/tips/ripgrep-integration/
[R42] Hacker News, "Everything is a filename search engine for Windows" - https://news.ycombinator.com/item?id=41337268
[R43] Brett Terpstra, "Mac File-Finding Gems" - https://brettterpstra.com/2025/05/02/mac-file-finding-gems/
[R44] Listary Discussions, "Search within the current folder" - https://discussion.listary.com/t/search-within-the-current-folder/8268
[R45] Listary Discussions, "Listary won't search recent folder/files" - https://discussion.listary.com/t/listary-wont-search-recent-folder-files/243
[R46] MacRumors, "EasyFind Versus Find Any File" - https://forums.macrumors.com/threads/easyfind-versus-find-any-file.2446693/
[R47] Hacker News, "What do y'all like for searching by file contents?" - https://news.ycombinator.com/item?id=30225910
[R48] MacUpdate, Find Any File - https://find-any-file.macupdate.com/
[R49] MacUpdate, EasyFind - https://easyfind.macupdate.com/
[R50] FSearch GitHub - https://github.com/cboxdoerfer/fsearch
[R51] voidtools forum, "Find Duplicates" - https://voidtools.com/forum/viewtopic.php?t=12795
[R52] voidtools forum, "Using Everything to help remove duplicates?" - https://voidtools.com/forum/viewtopic.php?t=7542
