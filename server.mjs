import { createServer } from "node:http";
import { readFile } from "node:fs/promises";

const port = Number.parseInt(process.env.PORT || "8080", 10);
const server = createServer(async (request, response) => {
  response.setHeader("cache-control", "no-store");
  response.setHeader("content-type", "text/plain; charset=utf-8");
  if (request.url === "/healthz") {
    response.end("ok\n");
    return;
  }
  if (request.url === "/canary") {
    try {
      response.end(await readFile(new URL("./src/canary.txt", import.meta.url), "utf8"));
    } catch {
      response.statusCode = 503;
      response.end("unavailable\n");
    }
    return;
  }
  if (request.url === "/") {
    response.end("Resilis deployment canary\n");
    return;
  }
  response.statusCode = 404;
  response.end("not found\n");
});
server.listen(port, "0.0.0.0");