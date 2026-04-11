# Deep Dive: Designing a Super-Fast, Space-Efficient Content Search Engine (2026)

Date: 2026-04-08  
Format: Markdown  
Audience: engineering leadership, search architects, platform engineers  
Bias: Rust-first, but pragmatic about mixing in other runtimes where they clearly win

---

## Executive summary

Your instinct is right: rebuilding every source file into one giant database is usually the wrong shape for a modern content search platform.

The current best practice is **not** "one storage system to rule them all". The current best practice is a **layered representation strategy**:

1. Keep the original files as the source of truth.
2. Build a **compact manifest/catalog** for discovery, governance, and filters.
3. Build a **lexical index** for fast exact, phrase, keyword, and fielded retrieval.
4. Build **multimodal sidecars** only where they create clear value: layout-aware page text, OCR/VLM text, tables, transcripts, thumbnails, image/video metadata.
5. Build **vector or learned sparse representations selectively**, not universally.

That is the key idea: **replicate representations, not payloads**.

The practical consequence is important:

- Do **not** store raw PDFs, DOCX files, images, and videos again inside the search engine unless you absolutely need instant full-document retrieval from the index itself.
- Do store the **minimum searchable derivative** for each workload.
- Use **different compression choices for different temperature tiers** instead of trying to find one magical format that is both maximally compressed and maximally fast.

The strongest Rust-first recommendation for your use case is this:

- **Primary architecture**: content-addressed source storage + Parquet/Arrow metadata lake + Quickwit/Tantivy for lexical retrieval + optional Qdrant or Lance for semantic/multimodal retrieval.
- **Parsing layer**: Apache Tika for broad format coverage, Docling for layout-aware document understanding, `ffprobe` for AV metadata, ExifTool for rich image/video metadata.
- **Compression policy**: LZ4 on the hottest query path, Zstandard on colder persisted data and repetitive small records, Parquet encodings for metadata columns, and vector quantization only where semantic retrieval is worth the storage.

If you execute this well, you can get very fast search **and** low storage amplification. The trick is to stop thinking of "the index" as one thing. It is several specialized compressed search structures sitting next to the original corpus.

---

## The core answer in one sentence

The cutting edge in 2026 is **object-store-native, layered, multimodal indexing with aggressive per-layer compression and selective semantic indexing**, not wholesale duplication of the corpus into a monolithic database.[QW1][QW2][AR1][PQ1][PQ2][LAN1][OS1]

---

## Why the giant database approach is usually wrong

A giant database feels simple, but it usually creates six problems:

1. **Storage amplification**: you keep the raw payload and then keep it again in row storage, document store, vector store, caches, thumbnails, and temporary parsing outputs.
2. **Slow parser upgrades**: if you improve your PDF parser or OCR, you often have to rehydrate and rewrite massive volumes.
3. **Poor modality fit**: text, metadata columns, images, embeddings, and transcripts all want different physical layouts.
4. **Hot/cold mismatch**: the query path needs tiny fast blocks; archival wants stronger compression.
5. **Operational lock-in**: one engine becomes responsible for ingestion, parsing, filters, lexical retrieval, vector search, storage, and analytics.
6. **Expensive semantic overbuild**: teams often embed everything before proving that the query workload actually needs it.

The alternative is better:

- Keep originals in place.
- Add **secondary search structures** on top.
- Version those search structures by **content hash + parser version + embedding version**.

This is the pattern behind the most efficient modern systems.

---

## The architectural principle that matters most

### Replicate representations, not payloads

For each source asset, create only the derivative artifacts that are useful:

- Text document:
  - extracted structural text
  - lexical index postings
  - maybe chunk embeddings
- PDF with tables and figures:
  - layout-aware text
  - table segments
  - page references
  - maybe page-image sidecars
- JPEG:
  - EXIF metadata
  - OCR/caption text if useful
  - maybe one image embedding
- Video:
  - container/stream metadata
  - transcript
  - keyframe captions or shot summaries
  - maybe sparse thumbnail set

That means the raw file is stored once, while the search system stores only compressed, query-optimized derivatives.

This is the single most important design choice if you want both speed and low storage overhead.

---

## What is actually cutting edge now

### 1. Search directly over external storage, not only local disks

Quickwit is a strong signal of where large-scale search architecture has gone: its design explicitly targets search and analytics directly on cloud/object storage with decoupled compute and storage.[QW1][QW2]

This matters because it breaks the old assumption that fast search requires keeping the entire indexed corpus on local SSDs. In the right design, you keep compact hot structures, use caches aggressively, and leave colder index files on external storage.

### 2. Use multiple physical representations for the same logical document

Modern search engines already do this internally:

- **inverted index** for text search
- **columnar/doc-values/fast fields** for filters, aggregations, sorting
- **row/doc store** for retrieval of stored fields

Quickwit exposes this cleanly with inverted index, fast fields, and doc store per field.[QW2] Tantivy similarly distinguishes compressed stored fields from column-oriented fast fields.[TAN1][TAN2]

The lesson: your platform should do the same **across the whole pipeline**, not just inside one engine.

### 3. Columnar metadata lakes are now a first-class part of search systems

Apache Arrow and Parquet are not just analytics tools. They are ideal for the **metadata side** of content search.[AR1][PQ1][PQ2]

Parquet gives you:

- dictionary encoding
- run-length / bit-packing hybrid encoding
- delta encodings
- column-level compression choices
- optional page index for page skipping

That makes Parquet excellent for:

- file manifests
- extraction outputs
- page-level metadata
- parser diagnostics
- quality scores
- enrichment outputs
- audit/history sidecars

DataFusion and DuckDB can then query those sidecars efficiently with projection and filter pushdown; DataFusion can even use Parquet page indexes and external access plans to skip row groups or pages.[DF1][DU1][PQ3]

### 4. Layout-aware and multimodal document processing has moved beyond OCR-only

For rich PDFs and office documents, OCR-only pipelines are now clearly behind best practice.

Docling is a good example of the current direction: it supports advanced PDF understanding, structured document representations, native chunkers, VLM-based end-to-end conversion, and multimodal export to Parquet.[DL1][DL2][DL3][DL4]

Recent research supports this direction. MMDocIR reports that visual retrievers outperform text-only counterparts for long documents, and that VLM-derived text beats OCR-only text for multimodal document retrieval.[MM1]

The practical takeaway: for visually rich documents, preserve **layout, element boundaries, and page context**.

### 5. Dense vectors are no longer the only semantic option

The current frontier is not just HNSW over float32 vectors.

Three relevant lines are now moving fast:

1. **Dense vectors with quantization** for memory reduction.[QD2][LAN2]
2. **Learned sparse retrieval** with ANN-like acceleration, such as SEISMIC.[RS1][OS1]
3. **Compressed embeddings** via Matryoshka-style training methods like SMEC.[SM1]

This matters because semantic retrieval no longer forces you to choose between "great recall" and "crazy storage bills". But the right answer depends heavily on workload.

---

## What best practice looks like for your project

## Recommended high-level architecture

```text
                 +---------------------------+
                 |  Original corpus          |
                 |  files / object store     |
                 |  pdf docx txt jpg mp4 ... |
                 +-------------+-------------+
                               |
                               v
                 +---------------------------+
                 |  Discovery + hashing      |
                 |  MIME detect              |
                 |  content hash             |
                 |  dedupe / versioning      |
                 +-------------+-------------+
                               |
               +---------------+----------------+
               |                                |
               v                                v
+-------------------------------+   +-------------------------------+
|  Extraction / enrichment      |   |  Metadata manifest lake       |
|  Tika / Docling / ffprobe     |   |  Arrow / Parquet              |
|  ExifTool / OCR / ASR / VLM   |   |  file rows + sidecar refs     |
+---------------+---------------+   +---------------+---------------+
                |                                   |
                v                                   v
+-------------------------------+   +-------------------------------+
|  Lexical search index         |   |  Analytics / admin / reindex  |
|  Quickwit or Tantivy          |   |  DataFusion / DuckDB          |
|  BM25 / phrase / facets       |   |  filters / quality / lineage  |
+---------------+---------------+   +-------------------------------+
                |
        +-------+--------+
        |                |
        v                v
+---------------+   +------------------------+
|  Query engine |   |  Optional semantic     |
|  hybrid rank  |   |  layer                 |
|  ACL filters  |   |  Qdrant or Lance       |
+-------+-------+   +-----------+------------+
        |                       |
        +-----------+-----------+
                    |
                    v
           +--------------------+
           |  Result assembly   |
           |  snippets / pages  |
           |  grouped by file   |
           +--------------------+
```

This architecture lets you keep storage overhead under control while still adding capabilities incrementally.

---

## The data model I would use

### 1. Source object record

One row per logical file:

- `source_id`
- `content_hash`
- `source_uri`
- `logical_path`
- `tenant_id`
- `acl_fingerprint`
- `mime_type`
- `bytes`
- `modified_at`
- `ingest_seen_at`
- `parser_version`
- `status`
- `duplicate_group_id`

### 2. Derived artifact record

One row per derivative artifact:

- `artifact_id`
- `source_id`
- `artifact_kind` (`text`, `page_text`, `table`, `image_meta`, `video_meta`, `transcript`, `thumbnail`, `embedding`, `ocr_text`, `vlm_text`, `parser_log`)
- `artifact_hash`
- `artifact_uri`
- `compression`
- `schema_version`
- `producer`
- `created_at`

### 3. Search chunk record

One row per searchable chunk:

- `chunk_id`
- `source_id`
- `page_start`
- `page_end`
- `section_path`
- `element_type`
- `text`
- `snippet_text`
- `language`
- `token_count`
- `metadata_filters`
- `embedding_ref` (nullable)

The important part is this: **the chunk record does not need to contain the original binary payload**.

---

## Parsing and enrichment: what to use

## Broad document coverage: Apache Tika

Apache Tika remains a very good broad-spectrum parser because it can detect and extract metadata and text from over a thousand file types through a single interface.[TK1]

Use Tika as your default parser-of-record for:

- PDF, Office, HTML, plain text, email, archives, many legacy formats
- MIME detection
- coarse metadata
- fallback text extraction

Use it as a **general parser service**, not as the only truth for high-value PDFs.

## Rich document understanding: Docling

Use Docling for documents where structure matters:

- visually rich PDFs
- tables
- figures
- page layout
- scanned documents
- hierarchical chunking
- tokenizer-aware chunking
- multimodal export to Parquet

Docling explicitly supports advanced PDF understanding, native chunkers, VLM pipelines, and Parquet-based multimodal exports.[DL1][DL2][DL3][DL4]

Best practice here is:

- use a **hierarchical or hybrid chunker** for prose
- use **line-preserving chunking** for tables, code, logs, and lists
- keep page numbers, captions, and headings as metadata

## Video and audio metadata: `ffprobe`

Use `ffprobe` for fast, structured AV metadata extraction. It can emit machine-readable JSON and is ideal for:

- codecs
- duration
- stream layout
- frame size
- bit rate
- frame rate
- audio channels
- subtitle streams

That is your low-cost metadata baseline for video/audio.[FF1]

## Rich image and embedded metadata: ExifTool

Use ExifTool where you need broad, high-fidelity metadata extraction across images and many embedded formats. JSON output is supported directly.[EX1]

That makes it a good fit for:

- EXIF/IPTC/XMP
- capture/device metadata
- timestamps
- embedded metadata in media files

## OCR and VLM usage policy

Do not OCR everything.

Use this decision rule:

1. If the file is text-native, prefer text extraction.
2. If the file is scanned or has weak text density, OCR only the necessary pages.
3. If the file is layout-heavy and high value, use layout-aware parsing and optionally a VLM path.

That gives you better quality and lower compute/storage cost than blanket OCR.

---

## Retrieval architecture: what wins in practice

## First-stage retrieval should still be lexical for most content search

This is the most important practical point in the whole report.

For a large heterogeneous enterprise corpus, **lexical retrieval remains the most storage-efficient default first-stage search layer**.

Why:

- exact tokens matter for filenames, paths, codes, names, part numbers, legal text, error messages, quoted phrases
- inverted indexes compress postings extremely well
- phrase search and fielded filters are mature and fast
- ACL and metadata filters fit naturally into fast fields/doc values

Lucene-style postings remain heavily compressed via packed integer blocks and variable-length integers.[LUC2] Tantivy and Quickwit inherit the same broad design philosophy around compact text indexing and separate stored/columnar data paths.[TAN1][TAN2][QW2]

### Recommended first-stage stack

- **Single node / embedded / library-first**: Tantivy
- **Distributed / object-store-native / very large immutable-ish corpus**: Quickwit

## Second-stage retrieval should be selective semantic retrieval, not universal embedding

Dense embeddings are valuable, but do not start by embedding every chunk of every file of every modality.

That is usually the fastest way to burn storage and complexity.

Use semantic retrieval in one or more of these roles:

1. **Reranking** the top lexical hits.
2. **Fallback semantic search** for natural-language concept queries.
3. **Selective semantic coverage** for high-value collections only.
4. **Multimodal retrieval** for image-rich or PDF-rich subcorpora.

This typically gives a better cost/performance balance than embedding the entire universe on day one.

## Learned sparse retrieval is the most interesting frontier for large text corpora

SEISMIC is important because it brings approximate retrieval ideas to learned sparse representations using inverted-list clustering, summaries, and a forward index.[RS1]

OpenSearch has already exposed this direction in production documentation through neural sparse ANN search on `sparse_vector` fields using SEISMIC, with quantization and hybrid segment behavior.[OS1][OS2]

Recent research then pushes on the remaining bottleneck: forward-index size. A 2026 paper on forward index compression for learned sparse retrieval reports that StreamVByte gave the best trade-off in their study and proposes DotVByte for further space savings while preserving efficiency.[RS3]

**My advice:** this is the right frontier to watch if your corpus grows into the tens or hundreds of millions of chunks and semantic text retrieval becomes central.

---

## Compression strategy by layer

There is no single best compression technique. The right answer depends on access pattern.

## The compression pyramid

### Cold layer: original payloads

Goal: low storage cost, low rewrite frequency.

Use:

- source-of-truth object storage or existing filesystem
- exact dedupe by content hash
- optional content-defined chunking only if you own the blob layer and versioned duplicates are common

### Warm layer: metadata and enrichment sidecars

Goal: compact scans, low-cost analytics, selective reads.

Use:

- Arrow/Parquet
- dictionary encoding + RLE/bit-packing + delta encodings where applicable
- Zstandard or LZ4_RAW depending write/read trade-off
- page index and row-group sizing for selective scans

Parquet is excellent here because it was designed around compact column encodings and selective reading.[PQ1][PQ2][PQ3]

### Hot layer: lexical index stored fields and caches

Goal: very fast online reads.

Use:

- engine-native postings compression
- small stored fields only
- LZ4 where latency dominates
- maybe low-level Zstd where storage matters more than p95 latency

Tantivy's store compresses blocks with LZ4 or Zstd.[TAN1] Lucene's stored fields use block compression with LZ4 by default and offer a stronger compression mode at slower speed.[LUC1]

### Semantic layer: vectors or sparse vectors

Goal: compress enough that semantic retrieval is affordable.

Use:

- scalar quantization as the default dense-vector compression
- product quantization when RAM is the hard limit
- binary / 1.5-bit / 2-bit quantization only for compatible embeddings and only with quality testing
- learned sparse retrieval when lexical/semantic fusion is central and you want inverted-index economics

---

## Exact recommendations by data type

| Layer | Recommended physical form | Default compression | Why |
|---|---|---:|---|
| Original files | existing filesystem or object store | leave as source of truth | avoid raw duplication |
| File manifest | Parquet | Zstd | highly repetitive columns compress well |
| Parser metadata JSON | Parquet or small blobs | Zstd + trained dictionaries | repetitive small records benefit a lot |
| Search snippets/stored fields | Quickwit/Tantivy doc store | LZ4 first | low-latency retrieval |
| Filters/facets/ACL/date fields | fast fields / doc values | engine-native columnar | fast filter/sort/agg |
| Full-text postings | engine-native inverted index | engine-native packed compression | best speed/space trade-off |
| Dense embeddings | vector store / Lance | SQ first, PQ if needed | best default memory reduction |
| Sparse embeddings | sparse vector / learned sparse | quantized summaries + compressed forward index | promising frontier |
| Page-level multimodal exports | Parquet | Zstd | supports reprocessing and analysis |
| Transcripts | Parquet / compressed text blobs | Zstd | high redundancy, good compression |

---

## The practical codec choices I recommend

## Zstandard for colder persisted structures and small repetitive records

Zstandard is a very strong default for persisted artifacts because it offers a wide speed/ratio trade-off, fast decode, and a dictionary mode that is particularly useful for families of small records.[ZS1]

Use it for:

- parser metadata blobs
- JSON sidecars
- transcripts
- Parquet metadata lakes
- archival extracted text
- versioned sidecars

**Important best practice:** train separate dictionaries for different families of small records.

For example:

- one dictionary for EXIF JSON
- one for `ffprobe` JSON
- one for Tika metadata JSON
- one for page-level OCR/VLM records

This often gives much better ratios than one global dictionary.

## LZ4 for the hottest online read path

LZ4 is still a great choice when decompression speed dominates. Its headline property remains very fast compression and extremely fast decode.[LZ1]

Use it for:

- stored fields on hot paths
- caches
- small block-compressed online snippet stores

## Engine-native compression for postings

Do **not** invent your own compression format around postings lists unless you are doing search-engine research.

Lucene-style packed postings already use compressed integer block layouts optimized for decode speed.[LUC2] You want those mature, optimized code paths.

## Parquet encodings for metadata columns

Use Parquet encodings instead of dumping metadata into JSON blobs whenever fields are queryable or repeated. Dictionary encoding, RLE/bit-packing, and delta encodings are exactly the right tools for metadata sidecars.[PQ1]

---

## Vector compression: the right defaults

## Default: scalar quantization

Qdrant documents scalar quantization as `float32 -> uint8`, reducing vector storage by 4x and often improving comparison speed via SIMD, with low observed accuracy loss in their experiments.[QD2]

This is the right default dense-vector compression for most production systems.

## Use product quantization when memory is the dominant constraint

Qdrant notes that product quantization can compress more strongly than scalar quantization, but at a larger accuracy cost and with less SIMD-friendly distance calculations.[QD2]

Use PQ when:

- your vector layer dominates memory cost
- you accept some recall loss
- you are willing to compensate with oversampling and reranking

Lance also reflects this industry pattern by offering IVF_PQ and related quantized vector layouts.[LAN2]

## Use binary quantization only with guardrails

Qdrant's binary quantization is extremely compact and fast, but it has model-compatibility caveats and works best with rescoring and oversampling.[QD2]

This is powerful, but it is not the first thing I would choose for a general content search system unless:

- the embedding model is known to behave well under BQ
- you have measured recall carefully
- you are willing to keep original vectors or good rescoring paths

## Storage math that matters

Ignoring graph/index overhead and looking only at raw vector payload size:

### 768 dimensions

- float32: `768 * 4 = 3072 bytes`
- uint8 scalar-quantized: `768 bytes`
- 2-bit: `192 bytes`
- 1-bit: `96 bytes`

### 1536 dimensions

- float32: `1536 * 4 = 6144 bytes`
- uint8 scalar-quantized: `1536 bytes`
- 2-bit: `384 bytes`
- 1-bit: `192 bytes`

That simple arithmetic explains why "embed everything" becomes expensive fast.

---

## Learned sparse retrieval: when to care

This is one of the most relevant research directions for your project.

Why?

Because learned sparse retrieval can preserve several favorable properties of classic inverted indexing while adding semantic matching power.

### The important signals from the literature

- The SEISMIC work shows an efficient approximate retrieval algorithm over learned sparse representations and reports strong efficiency relative to existing methods.[RS1]
- A 2025 scalability study points out that earlier work was often evaluated only on a few million documents and then evaluates sparse retrieval at **138M passages** on MsMarco-v2.[RS2]
- A 2026 follow-up specifically targets compression of the forward index, reporting that StreamVByte gave the best memory/latency/accuracy trade-off in their study and introducing DotVByte.[RS3]

### What this means for you

If your system eventually reaches:

- very large chunk counts
- significant semantic query demand
- strong pressure on vector RAM

then learned sparse retrieval is probably the most intellectually serious path to evaluate after classic lexical + dense hybrid.

It is not yet the universal default, but it is no longer niche research either.

---

## Multimodal documents: what to store and what not to store

## Store structure, not just strings

For PDFs and office docs, preserve:

- page number
- section path
- table boundaries
- captions
- reading order
- detected element type
- page-image reference if needed

Do not flatten everything into one giant text blob unless you only care about crude keyword search.

## For tables, code, and logs, chunk by line or structure

Docling's line-based token chunker is explicitly intended for structured content like tables, code, logs, and lists.[DL2]

That is exactly the right instinct for content search because it preserves the retrieval unit users actually care about.

## For images, start with metadata plus one semantic representation

For JPEGs and similar assets, the best low-cost starting point is:

- EXIF/XMP/IPTC metadata
- OCR/caption text if useful
- one image embedding only if the product truly needs image-semantic search

Do not create ten redundant derivatives before the first real query benchmark.

## For video, do not embed every frame by default

Default video sidecars should usually be:

- `ffprobe` metadata
- transcript / subtitles if available
- sparse keyframes or shot-level summaries only where the workload justifies them

Full frame-level embedding is one of the fastest ways to destroy your storage budget.

---

## Dedupe and versioning

## Exact dedupe: always do this

Hash every source payload and use the content hash as the base identity for all derived artifacts.

That gives you:

- exact duplicate detection
- parser result reuse
- embedding reuse
- stable provenance
- cheap change detection

## Chunk-level dedupe: do it only when the corpus justifies it

If you own the storage substrate and expect many near-duplicate or versioned files, content-defined chunking can reduce physical storage of raw payloads or extracted sidecars.

Two useful signals here:

- a 2024 study says the CDC state of the art is not obvious and needs careful workload-specific evaluation.[CDC1]
- FastCDC remains a strong practical algorithm, with its USENIX paper reporting chunking speed about 10x higher than Rabin-based CDC and about 3x higher than Gear/AE-based CDC while keeping a comparable deduplication ratio.[CDC2]

### My recommendation

Use this decision rule:

- **Do exact file-level dedupe by default**.
- **Add CDC only if** versioned duplicates are materially increasing your source-storage bill.

Many search platforms do not actually need CDC in phase one.

---

## Rust-first stack recommendations

## Option A: best overall fit for your stated constraints

### Quickwit + Parquet/DataFusion + Tika/Docling + optional Qdrant

This is my primary recommendation.

#### Why it fits

- Quickwit is Rust-based and designed for search on object storage with decoupled compute/storage.[QW1][QW2]
- Quickwit lets you choose which fields are indexed, stored, or mapped as fast fields, and `store_source` can stay `false`, which avoids storing the original JSON payload again inside the index.[QW2][QW3]
- Parquet/Arrow + DataFusion gives you a Rust-native metadata lake with predicate/projection pushdown and page skipping.[AR1][DF1][PQ3]
- Tika + Docling cover broad and rich document parsing.[TK1][DL1]
- Qdrant is Rust-based and gives you mature quantization options when you actually need a vector layer.[QD1][QD2]

#### What it looks like

- originals in object storage / filesystem
- manifest + sidecars in Parquet
- lexical chunks in Quickwit
- optional embeddings in Qdrant for selected collections
- application query layer merges lexical, filters, and semantic results

#### Where it shines

- very large corpora
- append-heavy or mostly immutable content
- low storage amplification
- Rust-first platform teams

#### Its main weakness

It is a layered system, not a single product. That is a feature for performance/cost, but it means more orchestration.

---

## Option B: the most elegant unified multimodal direction

### Lance / LanceDB as a multimodal table + retrieval layer

LanceDB positions itself as a multimodal lakehouse and emphasizes keeping multimodal data, metadata, and embeddings in the same table while adding new columns without copying existing data.[LAN1]

Lance itself exposes multiple quantized vector index types such as IVF_PQ, IVF_HNSW_SQ, IVF_SQ, and IVF_RQ.[LAN2] Lance also has full-text search constructs around BM25-style token retrieval.[LAN3]

#### Why this is attractive

- one table abstraction for raw data, metadata, features, embeddings
- strong fit for multimodal AI workflows
- nice evolution story as more derived columns appear

#### Why I still rank it second for your case

For classic enterprise content search, Lucene/Tantivy/Quickwit style lexical retrieval is still the more battle-tested center of gravity.

I would choose Lance as the primary center only if:

- multimodal/AI retrieval is the core product identity, or
- you strongly value one table abstraction over a best-of-breed layered stack

---

## Option C: embedded and simple for a smaller first deployment

### Tantivy + DuckDB/DataFusion + sidecar parsers

This is great when:

- you want a library-first deployment
- the corpus fits a single machine or modest sharding plan
- you want maximal control in Rust

Tantivy gives you compressed stored fields and fast fields.[TAN1][TAN2]
DuckDB is excellent for local analyst workflows over Parquet.[DU1]
DataFusion is the Rust-native execution path for production services.[DF1]

This is the fastest route to a serious prototype.

---

## Option D: if you relax the Rust bias for feature breadth

### Lucene / OpenSearch

If you decide that feature breadth and ecosystem maturity matter more than a Rust-centered stack, Lucene/OpenSearch is still one of the strongest broad platforms.

OpenSearch now documents neural sparse ANN via SEISMIC on `sparse_vector` fields.[OS1][OS2]
Lucene also exposes scalar-quantized vector utilities in current APIs.[LUC3]

I would only choose this path if you are comfortable with the operational weight and JVM-centric ecosystem.

---

## My concrete recommendation

### Recommendation for phase 1

Build this first:

- **Source of truth**: existing files or object storage
- **Manifest lake**: Parquet + Arrow schemas
- **Search core**: Quickwit for distributed, Tantivy for single-node/embedded
- **Extraction**: Tika + Docling + `ffprobe` + ExifTool
- **Query**: BM25 + field filters + phrase search + grouped results by source file
- **No universal vector indexing yet**

This gets you a highly capable system fast, with low duplication.

### Recommendation for phase 2

Add semantic retrieval only where it pays off:

- selected collections
- high-value chunks
- image-rich or concept-heavy corpora
- natural-language query paths with weak lexical recall

Use scalar-quantized embeddings first.

### Recommendation for phase 3

If semantic text search becomes a dominant workload at very large scale, evaluate:

- learned sparse retrieval / SEISMIC-like pipelines
- compressed forward-index techniques
- selective multimodal retrieval for document-heavy subsets

---

## A note on Quickwit field design

A good Quickwit index for content search should look like this conceptually:

```yaml
version: 0.8
index_id: docs
index_uri: s3://search-indexes/docs

doc_mapping:
  mode: strict
  store_source: false
  field_mappings:
    - name: tenant
      type: text
      tokenizer: raw
      fast: true
    - name: acl_group
      type: text
      tokenizer: raw
      fast: true
    - name: mime
      type: text
      tokenizer: raw
      fast: true
    - name: language
      type: text
      tokenizer: raw
      fast: true
    - name: modified_at
      type: datetime
      fast: true
    - name: page
      type: u64
      fast: true
    - name: path
      type: text
      tokenizer: raw
    - name: title
      type: text
      tokenizer: default
    - name: body
      type: text
      tokenizer: default
      record: position

search_settings:
  default_search_fields: [title, body, path]
```

The main design rule is simple:

- fields used for **filters/sort/ACL** become fast fields
- fields used for **search** become indexed text
- only fields needed for **snippets/result rendering** become stored fields

That is how you avoid storage blow-up.

---

## What not to do

1. **Do not store original binaries inside the lexical index** unless the product absolutely requires it.
2. **Do not embed every chunk of every modality in phase one**.
3. **Do not flatten rich PDFs into one giant text blob** if tables, page context, or layout matter.
4. **Do not use OCR as the default for text-native documents**.
5. **Do not place all queryable metadata into schemaless JSON blobs** when columnar sidecars would compress and filter better.
6. **Do not invent custom postings compression** unless search compression is literally your research topic.
7. **Do not dedupe across trust boundaries without thinking through tenancy/security side effects**.

---

## Benchmark plan you should run before committing the architecture

## Corpus slices

Create four representative evaluation slices:

1. plain text / markdown / code
2. PDFs and Office docs
3. images with metadata
4. video/audio with metadata and transcripts

## Query classes

Benchmark each of these separately:

- exact filename/path search
- phrase search
- keyword search with metadata filters
- natural-language concept search
- table/code/log lookup
- image/video metadata lookup
- multi-tenant / ACL-filtered search

## Metrics

Track at least:

- ingest throughput (files/sec, MB/sec)
- parser CPU seconds per GB by MIME family
- derived storage amplification ratio
- lexical index bytes per extracted token
- vector bytes per searchable chunk
- p50 / p95 / p99 query latency
- recall@k / MRR / nDCG on judged queries
- reindex cost after parser version bump

## Success thresholds I would use

- phase 1 storage amplification should stay dominated by lexical + metadata sidecars, not vectors
- p95 lexical queries should be decisively faster than semantic-only queries
- parser re-runs should be hash-versioned and incremental, not whole-corpus rewrites

---

## A realistic phased roadmap

| Phase | Goal | What you build | What you deliberately avoid |
|---|---|---|---|
| 0 | establish truth | hashing, MIME detection, manifest lake | embeddings |
| 1 | fast baseline search | lexical index, filters, snippets, grouped results | multimodal overbuild |
| 2 | rich docs | layout-aware parsing, table/page chunks, transcripts | full universal vectors |
| 3 | selective semantics | Qdrant or Lance on high-value subsets | embedding everything |
| 4 | frontier optimization | learned sparse, forward-index compression, CDC if needed | premature full-stack rewrite |

---

## Bottom line

If I were building this for real in a Rust-heavy environment, I would do the following:

### Build this first

- content-addressed source storage
- Parquet manifest lake
- Quickwit for lexical retrieval and field filters
- Tika + Docling + `ffprobe` + ExifTool enrichment pipeline
- no full source duplication in the index
- no universal vector layer yet

### Use this compression policy

- **LZ4** on hot online retrieval paths
- **Zstd** on colder persisted sidecars and repetitive small records
- **Parquet encodings** for metadata columns
- **scalar quantization** for the first semantic layer
- **PQ/BQ only after measurement**
- **FastCDC only if source-storage dedupe becomes important enough to justify complexity**

### Watch these research directions closely

- learned sparse retrieval at very large scale
- forward-index compression for sparse retrieval
- multimodal long-document retrieval
- compressed embedding training (Matryoshka/SMEC)
- semantic inverted indexing designs like UniDex

That combination gives you the best chance of reaching the state you described: **very fast search over a heterogeneous corpus with minimal unnecessary data replication**.

---

## References

### Search engines and storage

- [QW1] Quickwit documentation: https://quickwit.io/docs
- [QW2] Quickwit architecture: https://quickwit.io/docs/overview/architecture
- [QW3] Quickwit index configuration: https://quickwit.io/docs/configuration/index-config
- [TAN1] Tantivy store module: https://docs.rs/tantivy/latest/tantivy/store/index.html
- [TAN2] Tantivy fastfield module: https://docs.rs/tantivy/latest/tantivy/fastfield/index.html
- [LUC1] Lucene stored fields format: https://lucene.apache.org/core/9_10_0/core/org/apache/lucene/codecs/lucene90/Lucene90StoredFieldsFormat.html
- [LUC2] Lucene postings format: https://lucene.apache.org/core/9_8_0/core/org/apache/lucene/codecs/lucene90/Lucene90PostingsFormat.html
- [LUC3] Lucene quantization package: https://lucene.apache.org/core/10_2_0/core/org/apache/lucene/util/quantization/package-summary.html

### Columnar metadata and execution

- [AR1] Apache Arrow docs: https://arrow.apache.org/docs/index.html
- [PQ1] Parquet encodings: https://parquet.apache.org/docs/file-format/data-pages/encodings/
- [PQ2] Parquet compression: https://parquet.apache.org/docs/file-format/data-pages/compression/
- [PQ3] Parquet page index: https://parquet.apache.org/docs/file-format/pageindex/
- [DF1] DataFusion ParquetSource: https://docs.rs/datafusion/latest/datafusion/datasource/physical_plan/parquet/source/struct.ParquetSource.html
- [DU1] DuckDB Parquet docs: https://duckdb.org/docs/current/data/parquet/overview

### Parsing and multimodal extraction

- [TK1] Apache Tika: https://tika.apache.org/
- [DL1] Docling documentation: https://docling-project.github.io/docling/
- [DL2] Docling chunking: https://docling-project.github.io/docling/concepts/chunking/
- [DL3] Docling vision models: https://docling-project.github.io/docling/usage/vision_models/
- [DL4] Docling multimodal export: https://docling-project.github.io/docling/examples/export_multimodal/
- [FF1] ffprobe docs: https://ffmpeg.org/ffprobe.html
- [EX1] ExifTool docs: https://exiftool.org/exiftool_pod.html

### Compression

- [ZS1] Zstandard: https://facebook.github.io/zstd/
- [LZ1] LZ4: https://lz4.org/

### Unified multimodal / vector systems

- [LAN1] LanceDB docs: https://docs.lancedb.com/
- [LAN2] Lance vector indices: https://lance.org/format/table/index/vector/
- [LAN3] Lance full-text search: https://lance.org/format/table/index/scalar/fts/
- [QD1] Qdrant docs: https://qdrant.tech/documentation/
- [QD2] Qdrant quantization: https://qdrant.tech/documentation/manage-data/quantization/
- [OS1] OpenSearch neural sparse ANN search: https://docs.opensearch.org/3.5/vector-search/ai-search/neural-sparse-ann/
- [OS2] OpenSearch sparse_vector field: https://docs.opensearch.org/3.5/mappings/supported-field-types/sparse-vector/

### Research and benchmarks

- [RS1] Efficient Inverted Indexes for Approximate Retrieval over Learned Sparse Representations (Seismic): https://arxiv.org/html/2404.18812v1
- [RS2] Investigating the Scalability of Approximate Sparse Retrieval Algorithms to Massive Datasets: https://arxiv.org/abs/2501.11628
- [RS3] Forward Index Compression for Learned Sparse Retrieval: https://arxiv.org/abs/2602.05445
- [MM1] MMDocIR: Benchmarking Multimodal Retrieval for Long Documents: https://arxiv.org/abs/2501.08828
- [SM1] SMEC: Rethinking Matryoshka Representation Learning for Retrieval Embedding Compression: https://aclanthology.org/2025.emnlp-main.1332/
- [UN1] UniDex: Rethinking Search Inverted Indexing with Unified Semantic Modeling: https://arxiv.org/pdf/2509.24632
- [CDC1] A Thorough Investigation of Content-Defined Chunking Algorithms for Data Deduplication: https://arxiv.org/abs/2409.06066
- [CDC2] FastCDC (USENIX ATC 2016 paper PDF): https://www.usenix.org/system/files/conference/atc16/atc16-paper-xia.pdf

