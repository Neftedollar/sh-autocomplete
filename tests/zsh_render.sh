#!/bin/zsh
# Loaded by Rust integration test. Sources the adapter in test mode, manually
# populates state, calls _shac_render_menu, and prints POSTDISPLAY.

set -e

export SHAC_ZSH_TEST_MODE=1
ADAPTER="${1:?adapter path required}"
TIP_TEXT="${2:?tip text required}"
NO_TIPS="${3:-}"

if [[ -n "$NO_TIPS" ]]; then
  export SHAC_NO_TIPS=1
fi

# Provide minimal zsh stubs so functions that call zle don't fail outside ZLE.
function zle() { return 0; }
typeset -g POSTDISPLAY=""

source "$ADAPTER"

# Populate one fake candidate so render path runs.
_shac_menu_item_keys=("k1")
_shac_menu_insert_texts=("t1")
_shac_menu_displays=("d1")
_shac_menu_kinds=("k")
_shac_menu_sources=("s")
_shac_menu_descriptions=("desc1")
_shac_menu_selected_index=1
_shac_pending_tip_id="some_id"
_shac_pending_tip_text="$TIP_TEXT"

_shac_render_menu

print -- "$POSTDISPLAY"
