# UFFS licensing and commercialization strategy

Date: 2026-04-08
Status: research and recommendation memo
Audience: project owner, core maintainer, legal reviewer, future commercial lead
Format: Markdown

## 0. Framing

This memo is strategic product and licensing guidance, not legal advice. Final license text, contributor agreements, trademark policy, and commercial terms should be reviewed by qualified counsel in the jurisdictions where UFFS will be sold and distributed.

The target is clear:

- be open enough that developers, agents, enterprises, and partners do not stop at first contact;
- keep a real path to paying the bills;
- avoid trust-destroying surprises later;
- make the licensing map simple enough that procurement, contributors, and future acquirers can understand it in one pass.

My bottom-line recommendation is:

1. Keep the trust-critical UFFS platform truly open source.
2. Commercialize above the platform, not by playing games with the platform.
3. Use weak copyleft or permissive licensing on the platform, depending on how much leverage you want to retain.
4. Reserve source-available or proprietary terms for clearly non-core value-add layers.
5. Clean up the current mixed-license state before you make a public commercialization push.

For UFFS specifically, the best default is:

- `uffs-core`, `uffs-mft`, `uffs-daemon`, `uffs-client`, `uffs-cli`, `uffs-mcp`, `uffs-tui`, and the canonical wire/protocol stack under `MPL-2.0 OR LicenseRef-UFFS-Commercial`;
- public docs/specs/examples either under the same open license or, where you want maximum reuse, under Apache-2.0 / CC-BY-4.0;
- official paid desktop apps, enterprise modules, cloud/fleet services, and support contracts under proprietary or, selectively, Fair Source terms;
- strong trademark policy and brand control;
- a CLA for core contributions if you want community contributions to remain eligible for the commercial side of the dual-license path.

If your absolute top priority is maximum adoption over licensing leverage, the best alternative is:

- Apache-2.0 for the core platform;
- proprietary or Fair Source add-ons on top;
- trademarks, paid apps, support, and hosted services as the main monetization layer.

That second option is closer in spirit to the Pi / Earendil model: MIT core, Fair Source value-add, proprietary enterprise/cloud, trademark protection, and explicit promises not to close the core later. [10][11]

## 1. What matters for UFFS specifically

UFFS is not a generic web backend. It is a high-performance NTFS/MFT search platform that is becoming a daemon-first local system service with thin clients and an MCP adapter on top. The current architecture already separates the engine from the user-facing products unusually well: `uffs-daemon` owns indexing and search; `uffs-client` is a thin integration layer; `uffs-mcp` is the protocol adapter; the CLI and TUI are consumers, not the engine itself. [INT-1]

That matters because it gives you a natural commercialization boundary:

- the platform can stay open;
- the products on top can be paid;
- the hosted/team/fleet layers can be paid;
- OEM and enterprise exceptions can be sold without poisoning the core adoption funnel.

This is exactly the kind of project where "open platform, paid products" can work well.

It also means that if you put restrictive source-available terms on the core, you are restricting the exact layer that developers, agent builders, and partners need to trust before they will invest time in UFFS.

## 2. Current state: what you have today

The current repo is not starting from zero. It already shows a mixed licensing direction:

- you have Apache-2.0, MIT, and MPL-2.0 license texts present in the project materials; [INT-2][INT-3][INT-4]
- you have a proprietary legacy marker saying some files are still waiting to be migrated to `MPL-2.0 OR LicenseRef-TTAPI-Commercial`; [INT-5]
- you have a commercial license reference that explicitly positions the commercial side as the way to avoid copyleft obligations and obtain commercial distribution/support rights; [INT-6]
- you also have a SKY proprietary license text that says it covers all source code, documentation, and related materials in the repository unless otherwise specified, and that it is for internal SKY use only. [INT-7]

That last point is a very large trust and diligence problem if the repository is meant to attract outside interest.

A public prospect, enterprise evaluator, or contributor reading the current set of license texts can reasonably conclude all of the following:

- some of the repo is open;
- some of it is closed;
- some of it is internal-only;
- some of it still points at a commercial license reference named after something other than UFFS;
- the exact path-level boundary is not obvious.

That kind of ambiguity slows adoption more than almost any single license choice.

### Practical implication

Before you optimize the monetization model, fix the map.

The first commercialization win for UFFS is not choosing a clever license. It is making the repository legible.

## 3. Definitions and guardrails

### 3.1 Open source means more than source-visible

The Open Source Definition is still the line that matters if you want to say "open source" without causing pushback. It requires, among other things, free redistribution, source availability, permission to create derived works, and no discrimination against persons, groups, or fields of endeavor. [1]

That means:

- a license that blocks commercial use is not open source;
- a license that blocks competing services is not open source;
- a license that requires a separate commercial agreement for ordinary use is not open source.

This is why the OSI has been aggressively calling out "open washing" where restricted licenses are marketed as open source. [1][17]

### 3.2 Fair Source and DOSP are real categories, but they are not OSS

Fair Source explicitly presents itself as an alternative to closed source, not as Open Source. Its definition is: source is public to read, use/modify/redistribute is allowed with minimal business-protecting restrictions, and the code later converts to an OSI-approved license through Delayed Open Source Publication (DOSP). [8][9]

That can be a legitimate choice. It just must be named honestly.

### 3.3 Trademark is separate from copyright license

Open code does not mean open brand. Earendil's Pi licensing memo is explicit that trademark enforcement is a main protection mechanism while the core remains MIT. [10] The OSI and many commercial-open projects similarly separate code freedom from brand control. [6][15]

For UFFS, trademark control is not optional. It is part of the monetization moat.

## 4. Strategic goals translated into licensing requirements

Your stated goal was: open enough not to stop interest, while preserving commercial possibility.

That translates into six concrete requirements.

### Requirement A: Low-friction trial and adoption

A developer, security team, AI-tooling builder, or power user should be able to:

- clone it;
- run it locally;
- build with it;
- ship internal experiments;
- understand what is free and what is paid.

If they cannot do that in one sitting, you lose mindshare.

### Requirement B: No later betrayal of the core

The RoboVM story in Mario Zechner's post is the clearest warning sign in your source set: a closed turn on the core can be devastating, even if the community later forks and survives. [11]

So the UFFS rule should be:

- once code is released under the public core open-source license, it never becomes less open retroactively.

You can charge for future modules. You can keep some future modules closed. You should not retroactively close the common platform.

### Requirement C: Enterprise-friendly legal posture

The platform license should be something procurement teams can recognize and clear.

That favors:

- Apache-2.0;
- MPL-2.0;
- possibly MIT for smaller helper libraries.

It disfavors:

- custom source-available core licenses;
- strong-copyleft core licenses if enterprise embedding is important;
- a public repo with contradictory proprietary headers.

### Requirement D: Preserve room for paid applications and services

You want to be able to sell:

- polished official applications;
- enterprise add-ons;
- hosted or managed layers;
- OEM deals;
- support and indemnity.

The core license should not make those impossible.

### Requirement E: Preserve some leverage over private engine modifications

If you want to sell commercial exceptions to people who modify the engine privately, a weak copyleft license helps more than a permissive one.

### Requirement F: Keep the language honest

If you use source-available or Fair Source on any layer, do not call that layer open source.

## 5. How the main license families fit UFFS

### 5.1 MIT

MIT is simple, highly permissive, and excellent for maximum reuse. The attached MIT text allows use, copy, modify, merge, publish, distribute, sublicense, and sell, with only notice preservation and warranty disclaimer in the text itself. [INT-4]

Pros for UFFS:

- lowest friction for adoption;
- great for SDKs, examples, and lightweight ecosystem pieces;
- makes it very easy for third parties to build on top.

Cons for UFFS:

- the license text does not include an explicit patent grant, unlike Apache-2.0; [INT-4][6]
- almost no leverage against private forks or proprietary wrappers;
- very little commercial-exception leverage.

Verdict:

- good for examples, schemas, tiny helper libraries, maybe protocol definitions;
- not my first choice for the main engine stack if you want licensing-based monetization leverage.

### 5.2 Apache-2.0

Apache-2.0 is permissive like MIT, but with explicit patent protection and NOTICE handling. The attached Apache text includes an express patent grant and a patent-retaliation clause; Choose a License also summarizes Apache-2.0 as permissive with an express patent grant. [INT-2][18]

Sentry says Apache-2.0 is its default license of choice for new open projects because it is permissive, widely adopted, and includes patent protection, while MIT is used as a fallback when broader GPL-2.0 compatibility is needed. [7]

Pros for UFFS:

- very adoption-friendly;
- better enterprise comfort than MIT because of the explicit patent grant;
- perfect for ecosystem-building where you monetize outside the code license.

Cons for UFFS:

- still little leverage against private forks or proprietary wrappers;
- dual licensing is possible only for the code you actually own, but permissive licensing reduces the practical value of the commercial alternative.

Verdict:

- strongest option if your core strategy is ecosystem first, money above the core.

### 5.3 MPL-2.0

MPL-2.0 is weak or file-level copyleft. Mozilla describes it as designed to require sharing of modifications to MPL-covered files while still allowing combination with open or proprietary code with minimal restrictions. Choose a License similarly notes that larger works can be distributed under different terms while MPL-covered files and modifications stay under MPL. [2][3]

Pros for UFFS:

- still OSI-open and enterprise-recognizable;
- proprietary apps on top are still possible;
- modifications to covered engine files must be shared if distributed;
- explicit patent rights are present in the MPL text. [3][INT-3]

Cons for UFFS:

- slightly more friction than Apache/MIT;
- some adopters will avoid any copyleft, even weak copyleft.

Verdict:

- the best fit if you want a genuinely open platform plus some leverage over private engine modifications.

### 5.4 GPL / AGPL

Strong copyleft is powerful but broad. Uber's legal write-up notes that companies may avoid copyleft because it can force them to open source the resulting software, which can be incompatible with the business model. [6]

Pros for UFFS:

- strong reciprocity;
- commercial licensing can be meaningful where distributors need exceptions.

Cons for UFFS:

- high enterprise friction;
- can chill embedding and integration;
- AGPL is optimized around network distribution concerns more than desktop/system-tool adoption.

Verdict:

- not the right default for the UFFS core.

### 5.5 BUSL / BSL / FSL / FCL / ELv2 / other source-available families

These are real business tools, but they are not substitutes for an open source core if adoption is your first concern.

Examples:

- HashiCorp's BSL grants broad rights but restricts competitive hosted or embedded offerings, then converts to MPL-2.0 after four years. [14]
- MariaDB's BSL FAQ describes BSL as source-visible, free for non-production, with guaranteed later open-source conversion. [13]
- Elastic's ELv2 allows broad use/modification/redistribution but prohibits providing the product as a managed service, bypassing license keys, and removing notices. [15]
- Sentry uses FSL for the main Sentry and Codecov web apps. [7]
- Fair Source says its recommended FSL path is ideal for SaaS businesses, while FCL is the variant meant for self-hosted commercial features and license-key support. [8][19][20]

Pros for UFFS:

- strongest anti-free-rider leverage;
- good fit for paid self-hosted or cloud products when you still want to show source;
- DOSP can reduce downside risk if the business fails or abandons a product. [8][9]

Cons for UFFS:

- not open source;
- greater diligence friction and higher trust tax;
- can stop adoption at first contact if applied to the core;
- anti-service terms solve the wrong problem for a project whose primary value is a local engine and client stack.

Verdict:

- valid for non-core add-ons;
- bad default for the UFFS platform itself.

## 6. Recommendation matrix

Scoring is strategic, not legal, on a 1-5 scale.

| Model | Adoption | Trust / clarity | Commercial leverage | Fit for UFFS | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| Apache-2.0 core + proprietary apps/services | 5 | 5 | 3 | 5 | Excellent if adoption is priority one |
| MPL-2.0 core + proprietary apps/services | 4 | 5 | 4 | 5 | Best overall balance |
| MPL-2.0 OR commercial on core + paid products on top | 4 | 5 | 5 | 5 | Best overall if you want OEM/commercial exceptions too |
| MIT core + paid products on top | 5 | 5 | 2 | 4 | Good, but weaker moat than Apache/MPL |
| AGPL or GPL core + commercial exceptions | 2 | 4 | 4 | 2 | Too much friction for this project |
| FSL / FCL / BSL / ELv2 on core | 2 | 2 | 5 | 2 | Do not use on core |
| Closed-source core | 1 | 1 | 4 | 1 | Opposite of the stated goal |

## 7. The recommended UFFS licensing architecture

## 7.1 Core principle

Keep the UFFS platform open. Sell the official products, services, support, and exceptions around it.

### Public promise

Make this promise explicit in the repo and on the website:

> All code released as part of UFFS Core under the public open-source license will remain available under that license forever. Future commercial products may include additional code under other terms, but previously released UFFS Core code will not be made less open retroactively.

That single sentence will do more for trust than any clever license structure.

## 7.2 Recommended layer map

| Layer | Examples | Recommended license | Why |
| --- | --- | --- | --- |
| UFFS Core platform | `uffs-core`, `uffs-mft`, `uffs-daemon`, `uffs-client`, `uffs-cli`, `uffs-mcp`, `uffs-tui`, protocol implementation | `MPL-2.0 OR LicenseRef-UFFS-Commercial` | True OSS, allows proprietary apps on top, preserves some leverage over private changes |
| Public protocol/specs/examples | schemas, wire docs, sample integrations, maybe tiny SDKs | Apache-2.0 or MIT | Maximize ecosystem reuse and integration comfort |
| Documentation | user docs, architecture docs, website docs | CC-BY-4.0 or Apache-2.0 for code-like docs | Easy reuse without confusing code licensing |
| Official paid desktop apps | `uffs-studio`, investigator UI, premium workflows | Proprietary | This is product UI/UX value, not platform value |
| Enterprise modules | content indexing, compliance, fleet features, forensic packs | Proprietary by default; optionally FCL if showing source helps sales | Keep clear distinction from core |
| Hosted/team/fleet service | remote indexing control plane, collaboration, cloud workflows | Proprietary service terms | Natural monetization layer |
| OEM embedding exception | commercial rights for vendors who modify/embed core privately | Commercial license | Classic dual-license revenue path |

## 7.3 Why this is the right boundary for UFFS

Because your architecture is already daemon-first, the natural open boundary is the daemon/client/protocol platform. [INT-1]

That lets you sell products such as:

- an official pro GUI without closing the engine;
- a fleet or multi-host service without closing the engine;
- enterprise-only indexing, policy, or forensic modules without closing the engine;
- OEM rights for companies that want to embed and privately modify the engine.

In other words, UFFS is already architected for open platform plus paid product.

## 8. Why I recommend MPL-2.0 OR commercial for the core

This is the strongest recommendation if you want both openness and licensing leverage.

### 8.1 It is still true open source

MPL-2.0 is OSI-approved and commercially usable. [2][3]

### 8.2 It does not block proprietary applications on top

MPL's file-level copyleft allows larger works under different terms, which means a proprietary desktop application or managed service layer can sit on top of the MPL core without forcing that whole product open. [2][3]

### 8.3 It gives you something real to sell

If a customer wants to:

- modify core engine files privately;
- embed the engine inside a commercial appliance;
- redistribute it in a way they do not want governed by MPL reciprocity;
- buy support, warranties, or indemnity;

then the commercial license has real value.

### 8.4 It is already close to your current direction

One of your current proprietary references literally says legacy files are waiting to be migrated to `MPL-2.0 OR LicenseRef-TTAPI-Commercial`. [INT-5] Finishing and cleaning that direction is lower risk than inventing a new philosophy midstream.

### 8.5 It avoids the trust cliff of source-available core licenses

The moment your core engine becomes source-available instead of open source, the first conversation with advanced users changes from "how do I build on this?" to "what are the restrictions?" That is exactly the wrong first impression for a system tool and MCP platform.

## 9. The best alternative: Apache-2.0 core plus paid products on top

If you decide that UFFS needs the widest possible adoption funnel, use Apache-2.0 on the whole public platform and monetize almost entirely above it.

Why Apache over MIT for this path:

- Apache-2.0 is still permissive;
- it includes explicit patent rights; [18]
- Sentry uses Apache-2.0 as its default permissive outbound choice for that reason. [7]

This variant is closest to the Pi / Earendil posture:

- open core forever;
- optional Fair Source value-adds later;
- proprietary enterprise/cloud layers later;
- trademark as the primary protection layer. [10][11]

Use this variant if your instinct is:

> I care more about making UFFS the default file-search and agent-search platform than about monetizing commercial exceptions to the engine.

## 10. What should not be commercialized by license

To preserve trust, do not put these under source-available or proprietary terms if they are part of the public UFFS core proposition:

- the engine itself;
- the NTFS/MFT reading and indexing base layer;
- the daemon and public client stack;
- the public MCP server and public protocol surface;
- the canonical search, filter, sort, aggregate, and response semantics.

Those are the things people need to trust before they invest in UFFS.

If you put paywalls or source-available restrictions here, you are charging at the wrong layer.

## 11. What can be commercialized safely

This is where UFFS has a lot of room.

### 11.1 Official desktop products

Examples:

- UFFS Studio: polished GUI, saved workspaces, charts, folder treemaps, duplicate review UI, previews, compare views;
- UFFS Investigator: forensic/time-line oriented UI, evidence packs, chain-of-custody exports, analyst workflows.

These can be fully proprietary while depending on the open platform.

### 11.2 Enterprise and team features

Examples:

- multi-host or fleet orchestration;
- web control plane and auth;
- shared saved searches and reports;
- policy packs for compliance and retention;
- SSO / SCIM / audit trails;
- enterprise connectors and ticketing integrations.

These are excellent proprietary or selectively Fair Source candidates.

### 11.3 Advanced indexing and analysis packs

Examples:

- content indexing for Office/PDF/email;
- OCR and document preview pipelines;
- duplicate verification at scale;
- PII or secrets detection packs;
- ransomware / anomalous-file-behavior heuristics;
- cross-drive or cross-host dedupe analytics.

These can be sold as proprietary modules or, if you want source visibility for buyer trust, under FCL.

### 11.4 OEM and embedded rights

Because UFFS is a strong technical engine, you can sell:

- embedding rights;
- private-modification rights;
- white-label rights;
- support and indemnity;
- long-term maintenance terms.

This is where the commercial side of `MPL-2.0 OR commercial` becomes concrete.

### 11.5 Hosted services

Even if UFFS starts local-first, the natural hosted products are:

- fleet search and reporting across endpoints;
- remote collection and indexing coordination;
- enterprise dashboards;
- managed evidence/archive search;
- managed agentic workflows on top of local UFFS nodes.

Those are service businesses, not license businesses.

## 12. Where Fair Source actually fits UFFS

Fair Source is not a bad idea. It is just the wrong default for the core.

Fair Source's own materials say:

- Fair Source works best for a company's core product where the company wants to retain roadmap and business-model control; [8]
- FSL is ideal for SaaS businesses; [20]
- FCL is the variant for self-hosted commercial features with license-key support. [19]

For UFFS, that implies:

- FSL is not a natural fit for the local platform core, because UFFS is not primarily an anti-hosted-clone problem;
- FCL can be a fit for self-hosted enterprise modules that you want to show source for while protecting paid functionality.

So the right place for Fair Source in UFFS is:

- optional, clearly non-core enterprise modules;
- not `uffs-core`, `uffs-daemon`, `uffs-client`, `uffs-cli`, or `uffs-mcp`.

### Recommended rule

If you ever ship Fair Source code in the UFFS universe:

- label it "Fair Source" or "source-available";
- never label it "open source";
- never put it in the same directory tree as the public core without very explicit boundaries.

## 13. Contribution model and inbound rights

This is the licensing issue most founder-led open-core projects get wrong.

If you want to maintain a real commercial dual-license option for community contributions, you need inbound rights that let you do that.

### 13.1 What this means in practice

For the dual-licensed core, choose one of these two models:

#### Model A: employee/contractor-only dual-licensed core

- outside PRs are accepted only into permissive peripheral repos, docs, examples, or clearly non-dual-licensed areas;
- the dual-licensed core itself is maintained only by people whose rights are already assigned or contractually controlled.

This is simple, but reduces community velocity.

#### Model B: CLA for the dual-licensed core

- contributors sign an individual or corporate CLA granting you broad rights to use, sublicense, and relicense contributions;
- DCO can still be used alongside the CLA for provenance attestation;
- this keeps the commercial option alive as community contributions arrive.

Qt is a visible example of a commercially-licensed / open-source project that requires a contribution agreement and obtains broad copyright and patent rights from contributors, including the right to sublicense contributions under terms of the company's choosing. [21]

### Recommendation

For UFFS core, if you keep the dual-license model, use a CLA.

Keep it narrow and honest:

- explain exactly why it exists;
- limit it to repos or directories that are actually dual-licensed;
- do not require it for trivial docs-only or typo-only contributions if you can avoid it.

### 13.2 Governance promise

To preserve community trust, pair the CLA with a public promise:

- the CLA exists to preserve dual-licensing optionality and enforceability;
- it is not a trap to later close previously-open UFFS Core;
- previously released open core remains open.

## 14. Trademark strategy

Trademark should be one of your primary commercial defenses.

### 14.1 Why it matters

Strong open code plus strong trademarks is the cleanest durable combination for projects like UFFS.

It allows:

- community forks;
- broad technical adoption;
- paid official binaries and support;
- protection against confusing white-label clones pretending to be you.

### 14.2 What to publish

Publish `TRADEMARKS.md` with:

- what "UFFS" and logos are protected marks;
- who may say "compatible with UFFS";
- who may not ship binaries using the official name/logo;
- how official signed binaries and support are identified.

### 14.3 Brand architecture suggestion

- `UFFS Core` - the open platform;
- `UFFS Studio` - paid official GUI;
- `UFFS Enterprise` - paid modules and support;
- `UFFS Cloud` - hosted products.

That brand structure is easier to explain than mixing one product name across conflicting licenses.

## 15. Repo hygiene: what you should fix immediately

This is the highest-leverage operational section in the whole memo.

### 15.1 Rename the commercial reference

`LicenseRef-TTAPI-Commercial` is a poor public-facing name for a UFFS commercial license. [INT-6]

Replace it with something project-specific such as:

- `LicenseRef-UFFS-Commercial`, or
- `LicenseRef-UFFS-Enterprise`.

### 15.2 Remove or isolate SKY internal-only licensing from the public tree

A public repo containing a license that says the repository is for internal SKY use only unless otherwise specified is a due-diligence red flag. [INT-7]

Best practice:

- move genuinely internal-only code to a private repo;
- or isolate it into an unmistakably separate proprietary repo/subtree with its own build path;
- do not leave ambiguous repo-wide wording in a public project.

### 15.3 Burn down the legacy proprietary backlog fast

Your current legacy proprietary marker explicitly says some files have not yet been migrated to the dual-license system. [INT-5]

Create a target date and finish that migration. A long-lived legacy bucket destroys clarity.

### 15.4 Adopt REUSE and path-based SPDX discipline

REUSE exists specifically to make licensing clear, machine-readable, and CI-enforceable across mixed-license projects. [16]

For UFFS that means:

- SPDX header in every source file;
- `LICENSES/` directory for all standard and custom license texts;
- `REUSE.toml` and CI checks;
- automated failure if a file lands without an SPDX identifier.

### 15.5 Publish a simple license matrix

Add `LICENSE-MATRIX.md` with a table like:

| Path | License | Notes |
| --- | --- | --- |
| `crates/uffs-core/**` | `MPL-2.0 OR LicenseRef-UFFS-Commercial` | Public core engine |
| `crates/uffs-mft/**` | `MPL-2.0 OR LicenseRef-UFFS-Commercial` | Public core engine |
| `crates/uffs-daemon/**` | `MPL-2.0 OR LicenseRef-UFFS-Commercial` | Public platform |
| `crates/uffs-client/**` | `MPL-2.0 OR LicenseRef-UFFS-Commercial` | Public platform |
| `crates/uffs-cli/**` | `MPL-2.0 OR LicenseRef-UFFS-Commercial` | Public platform |
| `crates/uffs-mcp/**` | `MPL-2.0 OR LicenseRef-UFFS-Commercial` | Public platform |
| `docs/**` | `CC-BY-4.0` or Apache-2.0 | Documentation |
| `products/uffs-studio/**` | `LicenseRef-UFFS-Proprietary` | Paid product |
| `products/uffs-enterprise/**` | `LicenseRef-UFFS-Proprietary` or FCL | Paid modules |
| `services/uffs-cloud/**` | Proprietary | Hosted service |

### 15.6 Publish the full commercial terms

Right now the commercial reference points at a full `LICENSE-COMMERCIAL` file. [INT-6]

That file needs to be:

- present;
- named for UFFS;
- understandable;
- referenced from `COMMERCIAL.md`.

### 15.7 Split public and private repos unless there is a strong reason not to

A mixed-license monorepo is possible, but separate repos are easier to understand and much harder to misuse.

My recommendation:

- public repo for UFFS Core;
- private repo(s) for UFFS Studio / Enterprise / Cloud;
- if needed, private submodules or package feeds for paid extensions.

## 16. Business models that fit UFFS best

Do not overfocus on license sales. For UFFS, the healthier business mix is likely multi-channel.

### Best revenue layers

1. Paid official applications
2. Enterprise modules and workflows
3. Hosted fleet/team services
4. OEM/commercial exception deals
5. Support, consulting, training, and indemnity
6. Signed binaries, updates, and long-term support tracks

### What the license primarily does

- removes adoption blockers;
- protects the commons enough to avoid immediate hollowing-out;
- leaves room to sell higher-level products.

In other words: the license is there to enable the business, not to be the business.

## 17. Messaging: how to say this publicly

Use language like this:

### Public core announcement draft

> UFFS Core is open source and will remain open source. The search engine, daemon, client stack, CLI, MCP adapter, and protocol surface are the public platform. Some future official products and enterprise modules built on top of UFFS Core may be released under commercial or source-available terms, but code already released as part of UFFS Core will not be made less open retroactively.

### Fair Source announcement draft

> This module is source-available / Fair Source, not open source. The source is available to read and modify under the stated terms, and it converts to an open-source license on the published schedule.

### Trademark draft

> Fork the code if you want. Use the UFFS name and official logos only under the trademark policy.

That is the right level of directness.

## 18. What to avoid

### Avoid #1: putting non-OSS terms on the core

This directly conflicts with your stated adoption goal.

### Avoid #2: open-washing

If it has field-of-use, service-competition, or similar restrictions, do not call it open source. [1][17]

### Avoid #3: retroactive closure of the core

It will trigger exactly the kind of trust collapse Mario's RoboVM story warns about. [11]

### Avoid #4: keeping contradictory proprietary notices in public code for too long

This will spook enterprises faster than MPL ever will.

### Avoid #5: relying only on dual-license sales

If the main product value is the polished app or service, then the business must actually ship the polished app or service.

### Avoid #6: a CLA with no accompanying trust promise

People tolerate CLAs far more when you explain why they exist and what you will not do with them.

## 19. Recommended 90-day execution plan

### Phase 1: choose the public-core license line

Pick one of these and stop wavering:

- `MPL-2.0 OR LicenseRef-UFFS-Commercial` for the core; or
- `Apache-2.0` for the core if maximizing adoption is priority one.

My recommendation remains the first.

### Phase 2: clean the repo

- rename `LicenseRef-TTAPI-Commercial` to a UFFS-specific identifier;
- move or isolate SKY/internal-only material;
- burn down legacy proprietary files;
- add SPDX headers everywhere;
- add REUSE CI.

### Phase 3: publish governance docs

Add:

- `LICENSE-MATRIX.md`
- `COMMERCIAL.md`
- `TRADEMARKS.md`
- `CONTRIBUTING.md`
- `CLA.md` or CLA link if using dual-licensed community contributions

### Phase 4: define the paid surface

Pick one paid product to make real, for example:

- UFFS Studio, or
- UFFS Enterprise for fleet search/compliance, or
- OEM/commercial embedding.

### Phase 5: make the promise public

State clearly that:

- core remains open;
- paid products exist above the core;
- any source-available modules are honestly labeled;
- trademarks protect the official brand.

## 20. Final recommendation

If I were optimizing for your exact stated goal, I would do this:

### Recommended structure

- UFFS Core platform: `MPL-2.0 OR LicenseRef-UFFS-Commercial`
- Public schemas/examples/docs: Apache-2.0 or CC-BY-4.0 as appropriate
- Official desktop apps: proprietary
- Enterprise/fleet/cloud layers: proprietary by default; selectively FCL only where source visibility helps the sale
- Trademark: reserved and enforced
- Contributor model for the dual-licensed core: CLA plus DCO
- Public promise: once core code is open, it stays open

### Why this is best

Because it is the best combined answer to all five questions:

1. Will people try UFFS? Yes.
2. Will enterprises understand it? Yes.
3. Can you sell products on top? Yes.
4. Can you sell exceptions where needed? Yes.
5. Can you avoid repeating the "surprise, the core is no longer open" failure mode? Yes, if you make the permanence promise explicit.

### When I would choose Apache instead

Only if you decide the strategic goal is:

> make UFFS the default platform everywhere, even if that weakens licensing-based monetization leverage.

That is a valid strategy. It is just a different one.

## 21. Short answer

- Do not use source-available terms on the UFFS core.
- Keep the platform truly open source.
- Monetize applications, enterprise modules, hosted services, support, and OEM rights above it.
- Finish the migration to a clean, path-scoped license map immediately.
- If you want the best balance of openness and leverage, use `MPL-2.0 OR commercial` for the core.
- If you want the widest possible adoption, use Apache-2.0 for the core and accept that most monetization will happen above the code license.

## Appendix A: a simple decision tree

### If your priority is maximum adoption

Choose:

- Apache-2.0 core
- proprietary or Fair Source add-ons
- strong trademark policy

### If your priority is balance

Choose:

- MPL-2.0 OR commercial core
- proprietary apps/services
- CLA for core contributions

### If your priority is anti-clone protection on a hosted product

Choose:

- keep the core open;
- use FSL/FCL/BSL only on the hosted or self-hosted premium layer, not the platform core.

## Appendix B: internal materials reviewed

- `DAEMON_SERVICE_ARCHITECTURE.md`
- `cli-overview.md`
- `FILTER_SORT_FEATURE_MATRIX.md`
- `AGGREGATION_ARCHITECTURE.md`
- `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`
- `Apache-2.0.txt`
- `MIT.txt`
- `MPL-2.0.txt`
- `LicenseRef-Proprietary.txt`
- `LicenseRef-SKY-Proprietary.txt`
- `LicenseRef-TTAPI-Commercial.txt`

## Appendix C: external sources

[1] Open Source Initiative, "The Open Source Definition" - https://opensource.org/osd

[2] Mozilla, "MPL 2.0 FAQ" - https://www.mozilla.org/en-US/MPL/2.0/FAQ/

[3] Choose a License, "Mozilla Public License 2.0" - https://choosealicense.com/licenses/mpl-2.0/

[4] Apache Software Foundation, "Apache License 2.0" - https://www.apache.org/licenses/LICENSE-2.0.html

[5] Choose a License, "MIT License" - https://choosealicense.com/licenses/mit/

[6] Uber Engineering, "What Every Engineer Should Know About Open Source Software Licenses and IP" - https://www.uber.com/ci/en/blog/oss-ip/

[7] Sentry, "Licensing" - https://open.sentry.io/licensing/

[8] Fair.io, "About Fair Source" and "FAQ" - https://fair.io/about/ and https://fair.io/faq/

[9] Open Source Initiative, "Delayed Open Source Publication" - https://opensource.org/delayed-open-source-publication

[10] Earendil RFC 0015, "Pi Licensing" - https://rfc.earendil.com/0015/

[11] Mario Zechner, "I've sold out" - https://mariozechner.at/posts/2026-04-08-ive-sold-out/

[12] Qt, "Qt Licensing" - https://www.qt.io/development/qt-framework/qt-licensing

[13] MariaDB, "Business Source License FAQ" - https://mariadb.com/bsl-faq-mariadb/

[14] HashiCorp, "Business Source License 1.1" - https://www.hashicorp.com/en/bsl

[15] Elastic, "FAQ on Elastic License 2.0" - https://www.elastic.co/licensing/elastic-license/faq

[16] REUSE / FSFE - https://reuse.software/

[17] Open Source Initiative, "Meta's LLaMa license is still not Open Source" - https://opensource.org/blog/metas-llama-license-is-still-not-open-source

[18] Choose a License, "Apache License 2.0" - https://choosealicense.com/licenses/apache-2.0/

[19] Fair Core License - https://fcl.dev/

[20] Fair.io, "Join" / FSL guidance - https://fair.io/join/

[21] Qt Contribution Agreement v1.2 - https://www.qt.io/hubfs/Contribute%20to%20Qt/Qt-ContributionLicenseAgreement_v1_2_FINAL.pdf?hsLang=en

## Appendix D: internal source notes

- [INT-1] `DAEMON_SERVICE_ARCHITECTURE.md`: current daemon-first architecture, crate boundaries, and measured warm/cold behavior.
- [INT-2] `Apache-2.0.txt`: attached Apache 2.0 license text.
- [INT-3] `MPL-2.0.txt`: attached MPL 2.0 text, including patent and larger-work provisions.
- [INT-4] `MIT.txt`: attached MIT license text.
- [INT-5] `LicenseRef-Proprietary.txt`: legacy proprietary marker saying some files are still pending migration to `MPL-2.0 OR LicenseRef-TTAPI-Commercial`.
- [INT-6] `LicenseRef-TTAPI-Commercial.txt`: current commercial reference and rights summary.
- [INT-7] `LicenseRef-SKY-Proprietary.txt`: internal-only SKY proprietary repository-wide default unless otherwise specified.
