import http from "http";
import fs from "fs";
import path from "path";
const PORT = process.env.PORT || 3000;
const DIST = "/app/dist";
const MIME = {
  ".html":"text/html",".js":"text/javascript",".css":"text/css",
  ".json":"application/json",".svg":"image/svg+xml",
  ".png":"image/png",".ico":"image/x-icon",".woff2":"font/woff2"
};
http.createServer((req, res) => {
  const url = req.url.split("?")[0];
  let fp = path.resolve(DIST, url === "/" ? "index.html" : "." + url);
  if (!fp.startsWith(DIST) || !fs.existsSync(fp) || fs.statSync(fp).isDirectory()) fp = path.join(DIST, "index.html");
  const ext = path.extname(fp);
  res.writeHead(200, {"Content-Type": MIME[ext] || "application/octet-stream"});
  fs.createReadStream(fp).pipe(res);
}).listen(PORT, () => console.log("Listening on " + PORT));
