// Exercise the generated artifact in an isolated browser; no non-local requests are permitted.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { chromium } from "playwright";

const root = path.resolve(process.argv[2] || "target/rendergraph-book");
const mime = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".woff2": "font/woff2", ".json": "application/json" };
const server = createServer(async (request, response) => {
    try {
        const pathname = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
        const file = path.resolve(root, `.${pathname === "/" ? "/index.html" : pathname}`);
        const relative = path.relative(root, file);
        if (relative.startsWith("..") || path.isAbsolute(relative)) throw new Error("Outside artifact");
        response.setHeader("Content-Type", mime[path.extname(file)] || "application/octet-stream");
        response.end(await readFile(file));
    } catch { response.writeHead(404).end(); }
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
let browser;
const errors = [];
let diagrams = 0;
try {
    browser = await chromium.launch();
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
    page.on("pageerror", error => errors.push(error.message));
    await page.route("**/*", route => {
        const url = new URL(route.request().url());
        if (url.origin !== origin) {
            errors.push(`External request: ${url.origin}`);
            return route.abort();
        }
        return route.continue();
    });
    for (const chapter of ["design/rendergraph-overview.html", "design/rendergraph-diagrams.html", "print.html"]) {
        await page.goto(`${origin}/${chapter}`);
        await page.waitForFunction(() => ["ready", "error"].includes(document.documentElement.dataset.mermaidStatus));
        const state = await page.evaluate(() => ({
            status: document.documentElement.dataset.mermaidStatus,
            figures: [...document.querySelectorAll(".diagram")].map(figure => ({
                state: figure.dataset.diagramState,
                title: figure.querySelector("svg title")?.textContent,
                description: figure.querySelector("svg desc")?.textContent,
                width: figure.querySelector("svg")?.viewBox.baseVal.width,
                height: figure.querySelector("svg")?.viewBox.baseVal.height,
            })),
        }));
        assert.equal(state.status, "ready", `${chapter}: Mermaid rendering failed`);
        assert.ok(state.figures.length, `${chapter}: no diagrams rendered`);
        for (const figure of state.figures) {
            assert.equal(figure.state, "ready");
            assert.ok(figure.title && figure.description && figure.width > 0 && figure.height > 0, `${chapter}: missing accessible SVG`);
        }
        diagrams += state.figures.length;
    }
    await page.goto(`${origin}/design/rendergraph-diagrams.html`);
    await page.waitForFunction(() => document.documentElement.dataset.mermaidStatus === "ready");
    await page.locator(".diagram-expand").first().click();
    await page.getByRole("dialog").waitFor({ state: "visible" });
    await page.getByRole("button", { name: "図を拡大", exact: true }).click();
    await page.getByRole("button", { name: "原寸", exact: true }).click();
    await page.getByRole("button", { name: "全体を表示", exact: true }).click();
    await page.keyboard.press("Escape");
    await page.waitForFunction(() => document.querySelectorAll(".diagram-canvas svg").length === Number(document.documentElement.dataset.mermaidCount));
    await page.locator(".diagram-expand").first().click();
    const print = await page.pdf({ format: "A4", printBackground: true });
    assert.equal(print.subarray(0, 4).toString(), "%PDF");
    assert.equal(await page.getByRole("dialog").isVisible(), false, "Printing must return the expanded SVG to the chapter");
    assert.equal(await page.locator(".diagram-canvas svg").count(), Number(await page.locator("html").getAttribute("data-mermaid-count")));
    await page.getByRole("button", { name: "Change theme" }).click();
    await page.locator("#mdbook-theme-navy").click();
    await page.waitForFunction(() => document.documentElement.classList.contains("navy") && document.documentElement.dataset.mermaidStatus === "ready");
    assert.equal(await page.locator('.diagram[data-diagram-state="error"]').count(), 0);
    await page.setViewportSize({ width: 390, height: 844 });
    await page.reload();
    await page.waitForFunction(() => document.documentElement.dataset.mermaidStatus === "ready");
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "Mobile page overflows horizontally");
    await page.emulateMedia({ media: "print" });
    assert.equal(await page.locator(".diagram-expand").first().isVisible(), false);
    assert.deepEqual(errors, []);
    console.log(`Rendered ${diagrams} diagram instances, including the print book; checked viewer, theme, mobile, print styles and local-only assets.`);
} finally {
    await browser?.close();
    await new Promise(resolve => server.close(resolve));
}
