#!/usr/bin/env python3
"""Source contract only: not GTK CSS parsing or native rendering coverage."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]
CSS = (ROOT / 'src/ui/reference_dialogs.css').read_text()
RULES = {}
for selectors, body in re.findall(r'([^{}]+)\{([^{}]*)\}', re.sub(r'/\*.*?\*/', '', CSS, flags=re.S)):
    for selector in selectors.split(','):
        RULES[selector.strip()] = body


class DialogControlsTheme(unittest.TestCase):
    def test_actual_preferences_use_covered_native_controls(self):
        source = (ROOT / 'src/ui/dialogs.rs').read_text()
        self.assertIn('Scale::with_range(Orientation::Horizontal', source)
        self.assertIn('adw::SwitchRow::builder()', source)

    def test_controls_use_existing_tokens(self):
        source = (ROOT / 'src/ui/reference_shell.rs').read_text()
        for token in set(re.findall(r'@[A-Z_]+@', CSS)):
            self.assertIn('"' + token + '"', source)
        for node in ['scale trough', 'scale highlight', 'scale slider', 'switch slider', 'scale value']:
            body = RULES['.forge-workspace-dialog ' + node]
            self.assertRegex(body, r'@[A-Z_]+@')
            self.assertNotRegex(body, r'#[0-9a-fA-F]{3,8}\b')

    def test_scope_does_not_restyle_system_dialogs(self):
        for selector in RULES:
            self.assertIn('.forge-workspace-dialog', selector)

    def test_switch_focus_is_inside_allocation(self):
        body = RULES['.forge-workspace-dialog switch:focus-visible']
        self.assertIn('outline: 2px solid @ACCENT@', body)
        self.assertIn('outline-offset: -2px', body)

    def test_disabled_and_focus_are_explicit(self):
        for node in ['scale:disabled highlight', 'scale:disabled slider', 'switch:disabled', 'switch:disabled slider', 'checkbutton:disabled check']:
            self.assertIn('.forge-workspace-dialog ' + node, RULES)
        for node in ['scale:focus-visible trough', 'switch:focus-visible']:
            self.assertIn('@ACCENT@', RULES['.forge-workspace-dialog ' + node])


if __name__ == '__main__':
    unittest.main()
