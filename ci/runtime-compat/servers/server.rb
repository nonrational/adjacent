require "socket"

port = Integer(ENV.fetch("PORT"))
body = "RUBY #{RUBY_VERSION}\n"
server = TCPServer.new("127.0.0.1", port)
loop do
  conn = server.accept
  conn.gets                                   # request line
  while (line = conn.gets) && line != "\r\n"; end  # drain headers
  conn.write "HTTP/1.1 200 OK\r\nContent-Length: #{body.bytesize}\r\nConnection: close\r\n\r\n#{body}"
  conn.close
end
