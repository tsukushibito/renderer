import { attachScene } from "./viewer.js";

const canvas = document.querySelector("canvas");
const sceneSelect = document.querySelector("#scene");
const presetSelect = document.querySelector("#preset");
const reset = document.querySelector("#reset");
const retry = document.querySelector("#retry");
const status = document.querySelector("#status");
const credits = document.querySelector("#credits");
const locationUrl = new URL(window.location.href);
const initialScene = locationUrl.searchParams.get("scene");
if (initialScene === "fixture") sceneSelect.add(new Option("Fixture", "fixture"));
if ([...sceneSelect.options].some(option => option.value === initialScene)) sceneSelect.value = initialScene;

let current, busy = false, leaving = false;
function showFailure(error) {
    status.textContent = error?.message ?? String(error);
    retry.hidden = false;
    reset.disabled = presetSelect.disabled = true;
}
function setAddress(scene, view) {
    locationUrl.searchParams.set("scene", scene);
    if (view) locationUrl.searchParams.set("view", view);
    else locationUrl.searchParams.delete("view");
    history.replaceState(null, "", locationUrl);
}

async function load(preset) {
    if (busy) return;
    busy = true;
    sceneSelect.disabled = reset.disabled = presetSelect.disabled = true;
    retry.hidden = true;
    status.textContent = "Opening scene…";
    credits.textContent = "";
    presetSelect.replaceChildren();
    const scene = sceneSelect.value;
    try {
        await current?.dispose();
        current = undefined;
        const source = ["fixture", "indirect-shadow", "indirect-shadow-off"].includes(scene) ? scene : new URL(`./scenes/${scene}/scene.json`, document.baseURI);
        let manifest = { defaultView: "", views: [], credits: "" };
        if (source instanceof URL) {
            const response = await fetch(source);
            if (!response.ok) throw new Error(`Could not load ${source.pathname} (HTTP ${response.status}). Package this scene in the site directory.`);
            manifest = await response.json();
        }
        if (leaving) return;
        current = await attachScene(canvas, source, {
            onProgress: message => { status.textContent = message; },
            onError: showFailure,
        });
        if (leaving) {
            await current.dispose();
            return;
        }
        const selected = manifest.views.includes(preset) ? preset : manifest.defaultView;
        for (const name of manifest.views) presetSelect.add(new Option(name, name));
        if (selected) {
            presetSelect.value = selected;
            await current.preset(selected);
        }
        await current.frame(performance.now());
        current.start();
        credits.textContent = manifest.credits;
        reset.disabled = presetSelect.disabled = !selected;
        setAddress(scene, selected);
        status.textContent = "";
        canvas.focus({ preventScroll: true });
    } catch (error) {
        await current?.dispose();
        current = undefined;
        showFailure(error);
    } finally {
        busy = false;
        sceneSelect.disabled = false;
    }
}

async function restorePreset() {
    try {
        await current.preset(presetSelect.value);
        setAddress(sceneSelect.value, presetSelect.value);
        canvas.focus({ preventScroll: true });
    } catch (error) { showFailure(error); }
}
sceneSelect.addEventListener("change", () => load());
presetSelect.addEventListener("change", restorePreset);
reset.addEventListener("click", restorePreset);
retry.addEventListener("click", () => load(locationUrl.searchParams.get("view")));
window.addEventListener("pagehide", event => {
    if (!event.persisted) {
        leaving = true;
        current?.dispose();
    }
});
load(locationUrl.searchParams.get("view"));
