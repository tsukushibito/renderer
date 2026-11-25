// Module workers import the host's generated glue and share its Wasm memory.
// Retain the Worker objects for the lifetime of this team.
export function createTeam(module, memory, glue, first, count) {
    const workers = [];
    let stopped = false;
    const stop = () => {
        stopped = true;
        for (const worker of workers) worker.postMessage("stop");
    };
    const ready = Promise.all(Array.from({length: count}, (_, i) => new Promise((resolve, reject) => {
        const worker = new Worker(new URL("?sashiko-worker", import.meta.url), {type: "module", name: "sashiko-cpu"});
        workers.push(worker);
        worker.onerror = event => {
            event.preventDefault();
            reject(new Error(event.message));
        };
        worker.onmessage = ({data}) => {
            if (data.error) reject(new Error(data.error));
            else resolve();
        };
        worker.postMessage({module, memory, glue, ticket: first + i, host: self.name});
    }))).then(() => {
        if (stopped) throw new Error("Job initialization cancelled");
        for (const worker of workers) worker.postMessage("run");
    }).catch(error => { stop(); throw error; });
    return {ready, stop};
}

if (new URL(import.meta.url).searchParams.has("sashiko-worker")) {
    // Install both handlers before awaiting imports: cancellation during
    // initialization must not terminate a Worker inside the shared allocator.
    let initialized;
    self.onmessage = async ({data}) => {
        if (typeof data === "object") {
            initialized = initialize(data);
            return;
        }
        self.onmessage = null;
        const state = await initialized;
        if (!state) return;
        try {
            if (data === "run") state.worker.serve();
            else state.worker.free();
            state.wasm.__wbindgen_thread_destroy();
        } catch (error) {
            // The owner of a named host can observe a trap even when the
            // host itself is blocked joining that child. Discard this memory.
            if (state.host) {
                const channel = new BroadcastChannel("sashiko-jobs:" + state.host);
                channel.postMessage(String(error));
                channel.close();
            }
            throw error;
        } finally {
            self.close();
        }
    };
}

async function initialize({module, memory, glue, ticket, host}) {
    let wasm;
    try {
        const pkg = await import(glue);
        wasm = await pkg.default({module_or_path: module, memory});
        const worker = pkg.SashikoWorker.claim(ticket);
        self.postMessage({ready: true});
        return {worker, wasm, host};
    } catch (error) {
        if (wasm) wasm.__wbindgen_thread_destroy();
        self.postMessage({error: String(error)});
        self.close();
    }
}
