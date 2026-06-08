const http = require('http');

const port = Number(process.env.PORT);
if (!port) {
  console.error('PORT not set — Adjacent should inject it');
  process.exit(1);
}

const server = http.createServer((req, res) => {
  const start = process.hrtime.bigint();
  res.on('finish', () => {
    const ms = Number(process.hrtime.bigint() - start) / 1e6;
    console.error(`[hello-node] ${req.method} ${req.url} ${res.statusCode} ${ms.toFixed(1)}ms`);
  });

  if (req.url === '/healthz') {
    res.writeHead(200, { 'Content-Type': 'text/plain' });
    res.end('ok\n');
    return;
  }
  res.writeHead(200, { 'Content-Type': 'text/plain' });
  res.end(`Hello from Node ${process.version} on :${port}\n`);
});

server.listen(port, '127.0.0.1', () => {
  console.error(`hello-node listening on 127.0.0.1:${port}`);
});

for (const sig of ['SIGTERM', 'SIGINT']) {
  process.on(sig, () => {
    console.error(`hello-node received ${sig}, shutting down`);
    server.close(() => process.exit(0));
  });
}
