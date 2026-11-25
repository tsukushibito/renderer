// Each dedicated host owns a separate Wasm memory and its pool of workers.
let example;
self.onmessage = async ({data}) => {
    try {
        if (data.type === "start") {
            const parallel = self.crossOriginIsolated && typeof SharedArrayBuffer === "function"
                && WebAssembly.validate(Uint8Array.of(0, 97, 115, 109, 1, 0, 0, 0, 5, 4, 1, 3, 1, 1));
            if (!parallel && data.requireParallel) {
                throw new Error("Parallel jobs require cross-origin isolation and shared Wasm memory.");
            }
            const pkg = await import(parallel ? "./parallel/jobs.js" : "./inline/jobs.js");
            await pkg.default();
            example = await pkg.Example.start(parallel ? data.parallelism : 1);
            postMessage({type: "ready", mode: example.parallel() ? "parallel" : "inline"});
        } else if (data.type === "release") {
            example.free();
            example = undefined;
            postMessage({type: "released"});
        } else if (data.type === "run") {
            postMessage({type: "running"});
            const [checksum, participants, hostParticipated] = example.calculate(data.rounds);
            const heap = Array.from(example.frame_heap());
            postMessage({type: "done", checksum, participants, hostParticipated, heap});
        }
    } catch (error) {
        postMessage({type: "error", message: String(error)});
    }
};
