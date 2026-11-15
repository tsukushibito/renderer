/* Mermaid rendering and an accessible, scrollable diagram viewer. */
(() => {
    "use strict";

    const blocks = [...document.querySelectorAll(".mermaid")];
    if (!blocks.length) return;

    const root = document.documentElement;
    const dark = () => ["navy", "coal", "ayu"].some(name => root.classList.contains(name));
    const records = blocks.map((block, index) => {
        const source = block.textContent.trim();
        const title = source.match(/^\s*accTitle\s*:\s*(.+)$/m)?.[1] || `図 ${index + 1}`;
        const figure = document.createElement("figure");
        figure.className = "diagram";
        figure.dataset.diagramState = "pending";
        figure.innerHTML = `<figcaption><span></span><button type="button" class="diagram-expand" disabled>拡大して表示</button></figcaption><div class="diagram-canvas" tabindex="0" role="region"></div><details class="diagram-source"><summary>Mermaid 定義</summary><pre><code></code></pre></details>`;
        figure.querySelector("figcaption span").textContent = title;
        const canvas = figure.querySelector(".diagram-canvas");
        canvas.setAttribute("aria-label", `${title}（横にスクロールできます）`);
        figure.querySelector("code").textContent = source;
        block.replaceWith(figure);
        return { source, title, figure, canvas };
    });

    const dialog = document.createElement("dialog");
    dialog.className = "diagram-dialog";
    dialog.setAttribute("aria-labelledby", "diagram-dialog-title");
    dialog.innerHTML = `<header><h2 id="diagram-dialog-title"></h2><button type="button" class="diagram-close">閉じる</button></header><div class="diagram-tools"><button type="button" data-zoom="out" aria-label="図を縮小">−</button><button type="button" data-zoom="in" aria-label="図を拡大">＋</button><button type="button" data-zoom="fit">全体を表示</button><button type="button" data-zoom="actual">原寸</button><output aria-live="polite"></output><span>スクロールで移動・Esc で閉じる</span></div><div class="diagram-viewport" tabindex="0" role="region" aria-label="拡大した図"></div>`;
    document.body.append(dialog);
    const viewport = dialog.querySelector(".diagram-viewport");
    let active = null;
    let scale = 1;

    function zoom(value) {
        if (!active) return;
        scale = Math.max(0.2, Math.min(4, value));
        active.svg.style.width = `${active.width * scale}px`;
        active.svg.style.height = `${active.height * scale}px`;
        dialog.querySelector("output").textContent = `${Math.round(scale * 100)}%`;
    }

    function fit() {
        if (active) zoom(Math.min(1, (viewport.clientWidth - 32) / active.width, (viewport.clientHeight - 32) / active.height));
    }

    function restore() {
        if (!active) return;
        active.svg.style.width = `${active.width}px`;
        active.svg.style.height = "auto";
        active.record.canvas.append(active.svg);
        active = null;
    }

    function closeViewer() {
        dialog.close();
        restore();
    }

    dialog.querySelector(".diagram-close").addEventListener("click", closeViewer);
    // mdBook handles Escape globally; keep the dialog's keyboard action local.
    dialog.addEventListener("keydown", event => {
        if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            closeViewer();
        }
    }, true);
    dialog.addEventListener("close", restore);
    dialog.addEventListener("cancel", event => {
        event.preventDefault();
        closeViewer();
    });
    dialog.querySelector('[data-zoom="out"]').addEventListener("click", () => zoom(scale / 1.25));
    dialog.querySelector('[data-zoom="in"]').addEventListener("click", () => zoom(scale * 1.25));
    dialog.querySelector('[data-zoom="actual"]').addEventListener("click", () => zoom(1));
    dialog.querySelector('[data-zoom="fit"]').addEventListener("click", fit);
    window.addEventListener("beforeprint", () => { if (dialog.open) closeViewer(); });

    records.forEach(record => {
        record.figure.querySelector(".diagram-expand").addEventListener("click", () => {
            const svg = record.canvas.querySelector("svg");
            if (!svg) return;
            const box = svg.viewBox.baseVal;
            active = { record, svg, width: box.width, height: box.height };
            // Move the SVG, preserving unique IDs for markers, titles and styles.
            viewport.append(svg);
            dialog.querySelector("h2").textContent = record.title;
            dialog.showModal();
            zoom(1);
            viewport.scrollTo(0, 0);
        });
    });

    let serial = 0;
    let previousDark = null;
    async function render() {
        if (dialog.open) closeViewer();
        const isDark = dark();
        previousDark = isDark;
        root.dataset.mermaidStatus = "rendering";
        mermaid.initialize({
            startOnLoad: false,
            securityLevel: "strict",
            layout: "dagre",
            theme: isDark ? "dark" : "default",
            fontFamily: '"Noto Sans JP", "Yu Gothic", Meiryo, sans-serif',
            flowchart: { htmlLabels: false, useMaxWidth: false },
            class: { htmlLabels: false, useMaxWidth: false },
            sequence: { useMaxWidth: false, wrap: false },
            state: { useMaxWidth: false },
        });
        let errors = 0;
        for (const record of records) {
            const button = record.figure.querySelector(".diagram-expand");
            button.disabled = true;
            record.figure.dataset.diagramState = "pending";
            try {
                const result = await mermaid.render(`renderer-diagram-${++serial}`, record.source);
                record.canvas.innerHTML = result.svg;
                const svg = record.canvas.querySelector("svg");
                svg.style.width = `${Math.ceil(svg.viewBox.baseVal.width)}px`;
                svg.style.height = "auto";
                svg.style.maxWidth = "none";
                svg.style.backgroundColor = isDark ? "#1b1b22" : "#ffffff";
                record.figure.dataset.diagramState = "ready";
                button.disabled = false;
            } catch (error) {
                errors++;
                record.figure.dataset.diagramState = "error";
                record.canvas.textContent = `図を描画できませんでした。Mermaid 定義を確認してください。${error.message}`;
            }
        }
        root.dataset.mermaidStatus = errors ? "error" : "ready";
        root.dataset.mermaidCount = String(records.length);
    }

    let queue = render();
    new MutationObserver(() => {
        if (dark() !== previousDark) queue = queue.then(render);
    }).observe(root, { attributes: true, attributeFilter: ["class"] });
})();
