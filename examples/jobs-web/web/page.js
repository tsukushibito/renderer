const element = id => document.getElementById(id);
let host;
let timer;
let failures;

function stop(message) {
    clearTimeout(timer);
    failures?.close();
    host?.terminate();
    host = undefined;
    element("start").disabled = false;
    element("run").disabled = element("stop").disabled = true;
    element("status").value = message;
}

element("start").onclick = () => {
    element("start").disabled = true;
    element("stop").disabled = false;
    element("status").value = "Starting";
    element("result").value = "";
    const name = crypto.randomUUID();
    const worker = new Worker(new URL("./host.js", import.meta.url), {type: "module", name});
    failures = new BroadcastChannel("sashiko-jobs:" + name);
    failures.onmessage = ({data}) => { if (host === worker) stop("Error: " + data); };
    host = worker;
    timer = setTimeout(() => stop("Error: worker initialization timed out"), 30000);
    worker.onerror = event => { if (host === worker) stop("Error: " + event.message); };
    worker.onmessage = ({data}) => {
        if (host !== worker) return;
        switch (data.type) {
        case "ready":
            clearTimeout(timer);
            element("status").value = data.mode;
            element("run").disabled = false;
            break;
        case "running":
            element("status").value = "Running";
            break;
        case "done":
            clearTimeout(timer);
            element("result").value = JSON.stringify(data);
            element("status").value = "Done";
            element("run").disabled = false;
            break;
        case "error":
            stop("Error: " + data.message);
            break;
        }
    };
    worker.postMessage({type: "start", parallelism: 2, requireParallel: element("required").checked});
};
element("run").onclick = () => {
    element("run").disabled = true;
    timer = setTimeout(() => stop("Error: job batch timed out"), 60000);
    host.postMessage({type: "run", rounds: 1000000});
};
element("stop").onclick = () => stop("Stopped");
