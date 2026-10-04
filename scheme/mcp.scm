;; mcp.scm — commands for the MCP server (src/mcp.rs, src/editor/mcp.rs).
;;
;; The server lets an AI assistant evaluate Steel in this editor and read its
;; buffers, messages, and docs — for debugging and package development. It is
;; OFF by default and only ever listens on 127.0.0.1 with a bearer token.
;; Configure with options (init.scm):
;;   (set-option "mcp-server" #t)            ; start at launch
;;   (set-option "mcp-server-port" 7811)     ; the default
;;   (set-option "mcp-server-token" "…")     ; default: generated, kept in mcp-token
;; `M-x mcp-server-info` (a builtin) shows the connection command and the log.

;; Start the MCP server (sets the `mcp-server` option for this session).
(define-command (mcp-server-start) (set-option "mcp-server" #t))

;; Stop the MCP server.
(define-command (mcp-server-stop) (set-option "mcp-server" #f))
