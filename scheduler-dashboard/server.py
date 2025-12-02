#!/usr/bin/env python3
"""
Simple proxy server that serves the dashboard and proxies SSE requests
to avoid CORS issues.

Usage:
    python3 server.py [--port PORT] [--scheduler-url URL]

Environment variables:
    PORT           - Server port (default: 8080)
    SCHEDULER_URL  - Scheduler URL (default: http://localhost:50051)
"""

import argparse
import http.server
import os
import socketserver
import urllib.request
import urllib.error
from pathlib import Path

# Configuration with environment variable fallbacks
DEFAULT_PORT = int(os.environ.get("PORT", 8080))
DEFAULT_SCHEDULER_URL = os.environ.get("SCHEDULER_URL", "http://localhost:50051")

# Will be set by argument parsing
SCHEDULER_URL = DEFAULT_SCHEDULER_URL

class ProxyHandler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(Path(__file__).parent), **kwargs)

    def do_GET(self):
        # Proxy requests to /scheduler/* to the actual scheduler
        if self.path.startswith('/scheduler/'):
            self.proxy_request()
        else:
            super().do_GET()

    def proxy_request(self):
        target_url = f"{SCHEDULER_URL}{self.path}"
        
        try:
            req = urllib.request.Request(target_url)
            # Forward headers (except Host)
            for header, value in self.headers.items():
                if header.lower() not in ('host', 'connection'):
                    req.add_header(header, value)
            
            response = urllib.request.urlopen(req)
            
            # Send response status
            self.send_response(response.status)
            
            # Forward response headers
            for header, value in response.getheaders():
                if header.lower() not in ('transfer-encoding', 'connection'):
                    self.send_header(header, value)
            
            # Add CORS headers
            self.send_header('Access-Control-Allow-Origin', '*')
            self.end_headers()
            
            # Stream the response body (important for SSE)
            try:
                while True:
                    chunk = response.read(1024)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass
                
        except urllib.error.URLError as e:
            self.send_error(502, f"Proxy error: {e.reason}")
        except Exception as e:
            self.send_error(500, f"Server error: {str(e)}")

    def do_OPTIONS(self):
        # Handle CORS preflight
        self.send_response(200)
        self.send_header('Access-Control-Allow-Origin', '*')
        self.send_header('Access-Control-Allow-Methods', 'GET, OPTIONS')
        self.send_header('Access-Control-Allow-Headers', '*')
        self.end_headers()

def parse_args():
    parser = argparse.ArgumentParser(
        description="Dashboard proxy server for NativeLink Scheduler"
    )
    parser.add_argument(
        "--port", "-p",
        type=int,
        default=DEFAULT_PORT,
        help=f"Server port (default: {DEFAULT_PORT}, env: PORT)"
    )
    parser.add_argument(
        "--scheduler-url", "-s",
        type=str,
        default=DEFAULT_SCHEDULER_URL,
        help=f"Scheduler URL (default: {DEFAULT_SCHEDULER_URL}, env: SCHEDULER_URL)"
    )
    return parser.parse_args()


def main():
    global SCHEDULER_URL
    
    args = parse_args()
    port = args.port
    SCHEDULER_URL = args.scheduler_url
    
    with socketserver.TCPServer(("", port), ProxyHandler) as httpd:
        print(f"🚀 Dashboard server running at http://localhost:{port}")
        print(f"📡 Proxying SSE requests to {SCHEDULER_URL}")
        print(f"Press Ctrl+C to stop")
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            print("\n👋 Server stopped")


if __name__ == "__main__":
    main()

