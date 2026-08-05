#!/usr/bin/env bash
# Debug helper script for forge

set -e

CMD="${1:-info}"

case "$CMD" in
    info)
        echo "🔍 forge Debug Information"
        echo "==========================="
        echo ""
        echo "📂 Paths:"
        echo "   Config: ~/.config/forge/config.toml"
        echo "   State: ~/.config/forge/tabs.state"
        echo "   Binary: $(which forge 2>/dev/null || echo 'Not in PATH')"
        echo ""
        echo "📊 State File:"
        if [ -f ~/.config/forge/tabs.state ]; then
            echo "   Size: $(wc -c < ~/.config/forge/tabs.state) bytes"
            echo "   Lines: $(wc -l < ~/.config/forge/tabs.state)"
            echo "   Content:"
            cat ~/.config/forge/tabs.state | head -10 | sed 's/^/      /'
        else
            echo "   (No state file)"
        fi
        echo ""
        echo "⚙️  Config File:"
        if [ -f ~/.config/forge/config.toml ]; then
            echo "   Exists: Yes"
            echo "   Mode: $(grep '^terminal_mode' ~/.config/forge/config.toml || echo 'default')"
            echo "   Theme: $(grep '^theme' ~/.config/forge/config.toml || echo 'default')"
        else
            echo "   (No config file - using defaults)"
        fi
        echo ""
        echo "🔧 Running Processes:"
        ps aux | grep forge | grep -v grep || echo "   (No forge processes)"
        ;;

    logs)
        echo "📜 Running forge with debug logs..."
        FORGE_LOG=debug target/release/forge
        ;;

    trace)
        echo "🔬 Running forge with trace logs..."
        FORGE_LOG=trace target/release/forge
        ;;

    state)
        echo "📊 Current State File:"
        if [ -f ~/.config/forge/tabs.state ]; then
            cat ~/.config/forge/tabs.state
        else
            echo "(No state file)"
        fi
        ;;

    clean-state)
        echo "🧹 Cleaning state file..."
        if [ -f ~/.config/forge/tabs.state ]; then
            rm ~/.config/forge/tabs.state
            echo "✅ State file removed"
        else
            echo "No state file to remove"
        fi
        ;;

    reset-config)
        echo "🔄 Resetting config to defaults..."
        if [ -f config.toml.example ]; then
            cp config.toml.example ~/.config/forge/config.toml
            echo "✅ Config reset to defaults"
        else
            echo "❌ config.toml.example not found"
        fi
        ;;

    valgrind)
        echo "🔬 Running with valgrind..."
        valgrind --leak-check=full --show-leak-kinds=all target/release/forge
        ;;

    strace)
        echo "🔍 Running with strace..."
        strace -o /tmp/forge-strace.log target/release/forge
        echo "Trace saved to /tmp/forge-strace.log"
        ;;

    *)
        echo "Usage: $0 {info|logs|trace|state|clean-state|reset-config|valgrind|strace}"
        echo ""
        echo "Commands:"
        echo "  info         - Show debug information"
        echo "  logs         - Run with debug logs"
        echo "  trace        - Run with trace logs"
        echo "  state        - Show current state file"
        echo "  clean-state  - Remove state file"
        echo "  reset-config - Reset config to defaults"
        echo "  valgrind     - Run with valgrind"
        echo "  strace       - Run with strace"
        exit 1
        ;;
esac
