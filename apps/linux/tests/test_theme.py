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
            self.assertEqual(dark.replace("0.", "."), self.semantic[role]["dark"].replace("0.", "."), name)

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
