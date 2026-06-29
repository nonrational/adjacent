const http = require("http");

const port = parseInt(process.env.PORT, 10);
const body = `NODE ${process.versions.node}\n`;
http
  .createServer((_req, res) => {
    res.writeHead(200, { "Content-Length": Buffer.byteLength(body) });
    res.end(body);
  })
  .listen(port, "127.0.0.1");
