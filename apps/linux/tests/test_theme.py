"""The GTK palette comes from the Pigeonpost Design Kit tokens (pigeonpost/design_tokens.json)."""
import importlib.util
import json
import re
import unittest
from pathlib import Path

PACKAGE = Path(__file__).resolve().parent.parent / "pigeonpost"
HAS_GTK = importlib.util.find_spec("gi") is not None


@unittest.skipUnless(HAS_GTK, "needs PyGObject (GTK 4 and libadwaita)")
class KitPalette(unittest.TestCase):
    def setUp(self):
        from pigeonpost import theme
        self.theme = theme
        self.semantic = json.loads((PACKAGE / "design_tokens.json").read_text())["semantic"]

    def test_names_carry_kit_roles_in_light_and_dark(self):
        for name, role in self.theme.ROLES.items():
            if isinstance(role, tuple):
                continue
            light, dark = self.theme.PALETTE[name]
            self.assertEqual(light.replace("0.", "."), self.semantic[role]["light"].replace("0.", "."), name)
            expected = self.theme.DESKTOP_DARK.get(name, self.semantic[role]["dark"])
            self.assertEqual(dark.replace("0.", "."), expected.replace("0.", "."), name)

    def test_dark_panes_are_distinguishable_and_text_keeps_aa(self):
        dark = self.theme.colours(True)

        def luminance(colour):
            channels = [int(colour[i:i + 2], 16) / 255 for i in (1, 3, 5)]
            channels = [c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4 for c in channels]
            return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2]

        def contrast(a, b):
            x, y = luminance(a), luminance(b)
            return (max(x, y) + 0.05) / (min(x, y) + 0.05)

        levels = ["sidebar", "column", "wash", "ground"]
        for lower, upper in zip(levels, levels[1:]):
            self.assertGreaterEqual(contrast(dark[upper], dark[lower]), 1.08, f"{upper} too close to {lower}")
        self.assertGreaterEqual(contrast(dark["rule"], dark["wash"]), 1.2)
        for surface in levels + ["hover", "selected", "chip", "theirs_fill", "raised", "field", "composer"]:
            for text in ("ink", "body", "secondary", "muted", "link"):
                self.assertGreaterEqual(contrast(dark[text], dark[surface]), 4.5, f"{text} on {surface}")

    def test_light_mode_is_the_kits(self):
        light = self.theme.colours(False)
        for name, role in self.theme.ROLES.items():
            if not isinstance(role, tuple):
                self.assertEqual(light[name].replace("0.", "."), self.semantic[role]["light"].replace("0.", "."), name)
        self.assertEqual(light["field_stroke"], "transparent")

    def test_brand_accent_is_signal_blue(self):
        self.assertEqual(self.theme.PALETTE["accent"], ("#086CFF", "#086CFF"))
        self.assertEqual(self.theme.colours(False)["mine_fill"], self.semantic["bubble-mine"]["light"])
        self.assertEqual(self.theme.colours(True)["mine_fill"], self.semantic["bubble-mine"]["dark"])

    def test_every_colour_a_stylesheet_uses_is_defined(self):
        # Load every module that adds CSS, then check each @pp_* reference against the palette.
        sources = [path.read_text() for path in PACKAGE.glob("*.py")]
        used = set(re.findall(r"@pp_([a-z_]+)", "\n".join(sources)))
        self.assertFalse(used - set(self.theme.PALETTE), "undefined colours")

    def test_stylesheet_defines_both_appearances(self):
        for dark in (False, True):
            css = self.theme.stylesheet(dark)
            self.assertIn("@define-color pp_accent #086CFF;", css)
            self.assertIn("@define-color accent_bg_color #086CFF;", css)
            self.assertNotRegex(css, r"rgba\([^)]*,\s*\.\d")  # GTK needs a leading zero on alpha
            self.assertIn("border-radius: 18px", css)       # kit bubble radius


if __name__ == "__main__":
    unittest.main()
