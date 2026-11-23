# Mainmatter Rust Assessment

> Please take no more than ~1 hour to complete this assignment. We’ll discuss your solutions in a call in a few days. Feel free to ask questions if the assignment is unclear at any point.

## Context

This repository contains a simple file-icon lookup library as may be used by IDEs for their project tree sidebar. `get_icon_for_file` and `get_icon_for_folder` accept a filepath and return the numeric ID of a corresponding icon if one is found. Each icon ID corresponds to an SVG icon in `./icons`.

## Assignment

1. Implement a web server than allows users to query for file/folder icons over the network. 
2. Write a short (<= 1 page) design/implementation note describing:
  - Design choices and tradeoffs you made (high-level architecture, web server choice, libraries you picked etc.)
  - How you would productionize this service (deployment, observability, etc.)
  
Keep your audience in mind. We’re not interested in a perfect solution but one where we see your thought-process and working style in action.
  
You may add these notes at the end of this file if you wish.

## Usage
`iconator-server` embeds the SVGs and lookup tables meaning the binary can be just copied and run on any Linux host (not tested on Windows or MacOS).

```sh
cargo test

# check CLI options
cargo run -- --help

# run on port 50000
cargo run --release -- --bind 127.0.0.1:50000

# client requests examples
curl --get --data-urlencode 'path=src/lib.rs' http://127.0.0.1:50000/icons/file
curl --get --data-urlencode 'path=project/src' http://127.0.0.1:50000/icons/folder
```

## Design
### API
An IDE already knows whether each project-tree entry is a file or a folder. Thus, 2 routes. Both routes return SVG bytes 
with `Content-Type: image/svg+xml`. A missing, empty, or basename-less path returns HTTP error code `400`, an unlisted 
icon `404`. We might also want to version SVGs if clients fetch icons separately (instead of fetching bundles).

#### Optimization opportunities
When IDE opens a folder with a lot of files, currently we need to make request per file/folder. This could be speed up
by introducing batched request.

### Performance optimizations
#### Filesystem access
All icons fit in memory, the lookup reads in-memory tables, so it needs no filesystem access. This increases binary size 
and requires a rebuild for artwork changes, but a single release carries matching IDs and images. A test checks every 
mapped ID against the bundle. An unknown name is an expected lookup miss (`404`). A mapped ID with missing artwork 
indicates a broken release (`500`).

#### Web server optimization opportunities
Tokio is used for async runtime with default settings. We could process more requests by pinning Tokio workers to CPU 
cores, and benchmarking alternative async runtimes like Tokio-uring, Monoio etc.

## Deployment in production
- If we control the IDE, we can just bundle the icons with it, that will have the highest performance.
- If this will be used as a public service, fetching versioned SVNs from a CDN is preferred. Opening a directory then 
  requires zero network requests to decide which icons it uses.
- Own web server: the current implementation  bundles icons and can be deployed on any server. So we could deploy it
  behind a HTTPS load balancer. Each replica can answer requests independently since the icons are already compiled
  into the binary (about 5MB), so no shared cache, DB etc. needed.

CI/CD: we already have a test for validating icon bundle (`all_mapped_ids_have_embedded_artwork`). For CD we would 
canary an image and compare metrics (see list of metrics below) and slowly increase traffic. Once threshold metric reached,
canary is the new default.

### Observability
- OpenObserve for a greenfield project
- Victoria stack or other legacy stack integration 

Metrics:
- HTTP errors
- request rate
- latency histograms
- latency (median, p99)
- throughput (all, per CPU core)
- CDN hit ratio (if CDN used)
- timeouts
- CPU and memory resources per replica
- route names for metric labels

### High availability
For production, I would deploy stateless replicas behind a load balancer. During rollout I would drain 
connections before stopping each replica. The existing tracing events record route, status, and handler time. I would
add request counts by route and status, latency histograms, and alerts for server errors. Client paths can reveal 
project names, so I would also omit query strings from proxy logs and metric labels.

### Scaling
To size the service, I would replay bursts like opening a project tree, with file hits, folder hits, and misses. I would 
measure client p99, timeouts, and CPU per successful response, then repeat with one replica down before changing the 
runtime or worker layout.
