import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import path from "node:path";

if (!process.env.GSRF_TEST_EXPORT_DIR) throw new Error("GSRF_TEST_EXPORT_DIR is required");
const root = path.resolve(process.env.GSRF_TEST_EXPORT_DIR);
const types = { ".html": "text/html", ".xml": "application/rss+xml", ".css": "text/css", ".mjs": "text/javascript", ".svg": "image/svg+xml", ".png": "image/png", ".json": "application/json" };
createServer(async (request, response) => {
  try {
    const pathname = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
    let file = path.resolve(root, `.${pathname}`);
    if (!file.startsWith(`${root}${path.sep}`) && file !== root) {
      response.writeHead(403).end(); return;
    }
    if ((await stat(file)).isDirectory()) file = path.join(file, "index.html");
    const body = await readFile(file);
    response.writeHead(200, { "Content-Type": types[path.extname(file)] || "application/octet-stream" }).end(body);
  } catch (error) {
    if (error.code === "ENOENT") response.writeHead(404).end("Not found");
    else { console.error(error); response.writeHead(500).end("Preview server error"); }
  }
}).listen(8080, "127.0.0.1");
