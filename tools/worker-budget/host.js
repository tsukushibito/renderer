// Run on a dedicated Worker so the browser event loop remains responsive.
import init, { Trial, serve } from "./pkg/trial.js";

const timer = new Int32Array(new SharedArrayBuffer(4));
self.pauseTrial = () => Atomics.wait(timer, 0, 0, 16);

self.onmessage = async ({data}) => {
    try {
        if (data.module) {
            if (data.failStart) {
                self.postMessage("injected startup failure");
                self.close();
                return;
            }
            const wasm = await init({module_or_path: data.module, memory: data.memory});
            self.postMessage("ready");
            self.onmessage = ({data: command}) => {
                if (command === "run") serve(data.ticket);
                wasm.__wbindgen_thread_destroy();
                self.postMessage("stopped");
                self.close();
            };
            return;
        }
        self.postMessage({phase: "initializing Wasm"});
        const module = await WebAssembly.compileStreaming(fetch("./pkg/trial_bg.wasm"));
        const wasm = await init({module_or_path: module});
        const run = async (cancel) => {
            self.postMessage({phase: "starting team"});
            const trial = data.shared ? Trial.shared(data.n) : await Trial.current(data.n);
            const workers = [];
            try {
                if (data.shared) {
                    for (let i = 0; i < data.n; i++) {
                        const worker = new Worker(import.meta.url, {type: "module"});
                        const ready = new Promise((resolve, reject) => {
                            worker.onmessage = ({data}) => data === "ready" ? resolve() : reject(Error(String(data)));
                            worker.onerror = event => reject(Error(event.message));
                        });
                        worker.postMessage({module, memory: wasm.memory, ticket: trial.first() + i,
                            failStart: cancel && i === data.n - 1});
                        workers.push({worker, ready});
                    }
                    const states = await Promise.allSettled(workers.map(w => w.ready));
                    const failed = states.find(s => s.status === "rejected");
                    for (const [index, w] of workers.entries()) {
                        if (states[index].status === "rejected") continue;
                        w.stopped = new Promise((resolve, reject) => {
                            w.worker.onmessage = ({data}) => data === "stopped" ? resolve() : reject(Error(String(data)));
                            w.worker.onerror = event => reject(Error(event.message));
                        });
                        w.worker.postMessage(failed ? "cancel" : "run");
                    }
                    if (failed && !(cancel && failed.reason.message === "injected startup failure")) throw failed.reason;
                    if (cancel && !failed) throw Error("startup failure was not injected");
                }
                if (!cancel) {
                    self.postMessage({name: "scope", result: Array.from(trial.verify())});
                    for (const [name, heavy, physics, jobs, idle] of [
                        ["light", false, true, true, false], ["contact", true, true, true, false],
                        ["idle", true, true, true, true], ["physics", true, true, false, false],
                        ["jobs", true, false, true, false],
                    ]) {
                        self.postMessage({phase: name});
                        const result = Array.from(trial.sample(heavy, physics, jobs, idle));
                        self.postMessage({name, result});
                    }
                    for (const [name, late] of [["survivor-early", false], ["survivor-endgame", true]]) {
                        self.postMessage({phase: name});
                        self.postMessage({name, result: Array.from(trial.survivor(late))});
                    }
                }
            } finally {
                trial.free();
                await Promise.all(workers.map(w => w.stopped));
            }
        };
        if (data.shared) await run(true);
        await run(false);
        self.postMessage("done");
        self.close();
    } catch (err) {
        self.postMessage({error: String(err)});
        self.close();
    }
};
