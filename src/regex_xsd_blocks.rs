//! Block escapes `\p{IsX}` / `\P{IsX}` (XSD Part 2: 1.0 Appendix F, 1.1
//! §G.4.2.3), resolved to code-point ranges.
//!
//! The Rust `regex` crate, which runs XSD patterns without the `xsd11`
//! feature, knows no `Is`-prefixed block names, so
//! [`convert_xml_pattern`](crate::regex_convert::convert_xml_pattern) writes
//! every block escape out as a character class built from this table.
//!
//! The table is the block table of the `regexml` engine used with the
//! `xsd11` feature (Unicode 15.1 `Blocks.txt`), so both builds give a block
//! name the same meaning. Its keys are the normalized block names of 1.1
//! §G.4.2.3: "the string of characters formed by stripping out white space
//! and underbar characters from the block name as given in \[Unicode
//! Database\], while retaining hyphens and preserving case distinctions";
//! lookups are exact, as in `regexml`. It also holds the Unicode 3.1 names
//! superseded since (`Greek`, `CombiningMarksforSymbols`), which §G.4.2.3
//! recommends keeping for XSD 1.0 schemas; `PrivateUse`, the third, spans
//! several ranges and is answered by [`block_ranges`] itself. The test
//! `blocks_agree_with_regexml` (`xsd11` feature) checks every entry against
//! `regexml`.

use crate::schema::model::XsdVersion;

/// Normalized block name → first and last code point, sorted by name.
const BLOCKS: &[(&str, (u32, u32))] = &[
    ("Adlam", (0x1E900, 0x1E95F)),
    ("AegeanNumbers", (0x10100, 0x1013F)),
    ("Ahom", (0x11700, 0x1174F)),
    ("AlchemicalSymbols", (0x1F700, 0x1F77F)),
    ("AlphabeticPresentationForms", (0xFB00, 0xFB4F)),
    ("AnatolianHieroglyphs", (0x14400, 0x1467F)),
    ("AncientGreekMusicalNotation", (0x1D200, 0x1D24F)),
    ("AncientGreekNumbers", (0x10140, 0x1018F)),
    ("AncientSymbols", (0x10190, 0x101CF)),
    ("Arabic", (0x0600, 0x06FF)),
    ("ArabicExtended-A", (0x08A0, 0x08FF)),
    ("ArabicExtended-B", (0x0870, 0x089F)),
    ("ArabicExtended-C", (0x10EC0, 0x10EFF)),
    ("ArabicMathematicalAlphabeticSymbols", (0x1EE00, 0x1EEFF)),
    ("ArabicPresentationForms-A", (0xFB50, 0xFDFF)),
    ("ArabicPresentationForms-B", (0xFE70, 0xFEFF)),
    ("ArabicSupplement", (0x0750, 0x077F)),
    ("Armenian", (0x0530, 0x058F)),
    ("Arrows", (0x2190, 0x21FF)),
    ("Avestan", (0x10B00, 0x10B3F)),
    ("Balinese", (0x1B00, 0x1B7F)),
    ("Bamum", (0xA6A0, 0xA6FF)),
    ("BamumSupplement", (0x16800, 0x16A3F)),
    ("BasicLatin", (0x0000, 0x007F)),
    ("BassaVah", (0x16AD0, 0x16AFF)),
    ("Batak", (0x1BC0, 0x1BFF)),
    ("Bengali", (0x0980, 0x09FF)),
    ("Bhaiksuki", (0x11C00, 0x11C6F)),
    ("BlockElements", (0x2580, 0x259F)),
    ("Bopomofo", (0x3100, 0x312F)),
    ("BopomofoExtended", (0x31A0, 0x31BF)),
    ("BoxDrawing", (0x2500, 0x257F)),
    ("Brahmi", (0x11000, 0x1107F)),
    ("BraillePatterns", (0x2800, 0x28FF)),
    ("Buginese", (0x1A00, 0x1A1F)),
    ("Buhid", (0x1740, 0x175F)),
    ("ByzantineMusicalSymbols", (0x1D000, 0x1D0FF)),
    ("CJKCompatibility", (0x3300, 0x33FF)),
    ("CJKCompatibilityForms", (0xFE30, 0xFE4F)),
    ("CJKCompatibilityIdeographs", (0xF900, 0xFAFF)),
    ("CJKCompatibilityIdeographsSupplement", (0x2F800, 0x2FA1F)),
    ("CJKRadicalsSupplement", (0x2E80, 0x2EFF)),
    ("CJKStrokes", (0x31C0, 0x31EF)),
    ("CJKSymbolsandPunctuation", (0x3000, 0x303F)),
    ("CJKUnifiedIdeographs", (0x4E00, 0x9FFF)),
    ("CJKUnifiedIdeographsExtensionA", (0x3400, 0x4DBF)),
    ("CJKUnifiedIdeographsExtensionB", (0x20000, 0x2A6DF)),
    ("CJKUnifiedIdeographsExtensionC", (0x2A700, 0x2B73F)),
    ("CJKUnifiedIdeographsExtensionD", (0x2B740, 0x2B81F)),
    ("CJKUnifiedIdeographsExtensionE", (0x2B820, 0x2CEAF)),
    ("CJKUnifiedIdeographsExtensionF", (0x2CEB0, 0x2EBEF)),
    ("CJKUnifiedIdeographsExtensionG", (0x30000, 0x3134F)),
    ("CJKUnifiedIdeographsExtensionH", (0x31350, 0x323AF)),
    ("CJKUnifiedIdeographsExtensionI", (0x2EBF0, 0x2EE5F)),
    ("Carian", (0x102A0, 0x102DF)),
    ("CaucasianAlbanian", (0x10530, 0x1056F)),
    ("Chakma", (0x11100, 0x1114F)),
    ("Cham", (0xAA00, 0xAA5F)),
    ("Cherokee", (0x13A0, 0x13FF)),
    ("CherokeeSupplement", (0xAB70, 0xABBF)),
    ("ChessSymbols", (0x1FA00, 0x1FA6F)),
    ("Chorasmian", (0x10FB0, 0x10FDF)),
    ("CombiningDiacriticalMarks", (0x0300, 0x036F)),
    ("CombiningDiacriticalMarksExtended", (0x1AB0, 0x1AFF)),
    ("CombiningDiacriticalMarksSupplement", (0x1DC0, 0x1DFF)),
    ("CombiningDiacriticalMarksforSymbols", (0x20D0, 0x20FF)),
    ("CombiningHalfMarks", (0xFE20, 0xFE2F)),
    ("CombiningMarksforSymbols", (0x20D0, 0x20FF)),
    ("CommonIndicNumberForms", (0xA830, 0xA83F)),
    ("ControlPictures", (0x2400, 0x243F)),
    ("Coptic", (0x2C80, 0x2CFF)),
    ("CopticEpactNumbers", (0x102E0, 0x102FF)),
    ("CountingRodNumerals", (0x1D360, 0x1D37F)),
    ("Cuneiform", (0x12000, 0x123FF)),
    ("CuneiformNumbersandPunctuation", (0x12400, 0x1247F)),
    ("CurrencySymbols", (0x20A0, 0x20CF)),
    ("CypriotSyllabary", (0x10800, 0x1083F)),
    ("Cypro-Minoan", (0x12F90, 0x12FFF)),
    ("Cyrillic", (0x0400, 0x04FF)),
    ("CyrillicExtended-A", (0x2DE0, 0x2DFF)),
    ("CyrillicExtended-B", (0xA640, 0xA69F)),
    ("CyrillicExtended-C", (0x1C80, 0x1C8F)),
    ("CyrillicExtended-D", (0x1E030, 0x1E08F)),
    ("CyrillicSupplement", (0x0500, 0x052F)),
    ("Deseret", (0x10400, 0x1044F)),
    ("Devanagari", (0x0900, 0x097F)),
    ("DevanagariExtended", (0xA8E0, 0xA8FF)),
    ("DevanagariExtended-A", (0x11B00, 0x11B5F)),
    ("Dingbats", (0x2700, 0x27BF)),
    ("DivesAkuru", (0x11900, 0x1195F)),
    ("Dogra", (0x11800, 0x1184F)),
    ("DominoTiles", (0x1F030, 0x1F09F)),
    ("Duployan", (0x1BC00, 0x1BC9F)),
    ("EarlyDynasticCuneiform", (0x12480, 0x1254F)),
    ("EgyptianHieroglyphFormatControls", (0x13430, 0x1345F)),
    ("EgyptianHieroglyphs", (0x13000, 0x1342F)),
    ("Elbasan", (0x10500, 0x1052F)),
    ("Elymaic", (0x10FE0, 0x10FFF)),
    ("Emoticons", (0x1F600, 0x1F64F)),
    ("EnclosedAlphanumericSupplement", (0x1F100, 0x1F1FF)),
    ("EnclosedAlphanumerics", (0x2460, 0x24FF)),
    ("EnclosedCJKLettersandMonths", (0x3200, 0x32FF)),
    ("EnclosedIdeographicSupplement", (0x1F200, 0x1F2FF)),
    ("Ethiopic", (0x1200, 0x137F)),
    ("EthiopicExtended", (0x2D80, 0x2DDF)),
    ("EthiopicExtended-A", (0xAB00, 0xAB2F)),
    ("EthiopicExtended-B", (0x1E7E0, 0x1E7FF)),
    ("EthiopicSupplement", (0x1380, 0x139F)),
    ("GeneralPunctuation", (0x2000, 0x206F)),
    ("GeometricShapes", (0x25A0, 0x25FF)),
    ("GeometricShapesExtended", (0x1F780, 0x1F7FF)),
    ("Georgian", (0x10A0, 0x10FF)),
    ("GeorgianExtended", (0x1C90, 0x1CBF)),
    ("GeorgianSupplement", (0x2D00, 0x2D2F)),
    ("Glagolitic", (0x2C00, 0x2C5F)),
    ("GlagoliticSupplement", (0x1E000, 0x1E02F)),
    ("Gothic", (0x10330, 0x1034F)),
    ("Grantha", (0x11300, 0x1137F)),
    ("Greek", (0x0370, 0x03FF)),
    ("GreekExtended", (0x1F00, 0x1FFF)),
    ("GreekandCoptic", (0x0370, 0x03FF)),
    ("Gujarati", (0x0A80, 0x0AFF)),
    ("GunjalaGondi", (0x11D60, 0x11DAF)),
    ("Gurmukhi", (0x0A00, 0x0A7F)),
    ("HalfwidthandFullwidthForms", (0xFF00, 0xFFEF)),
    ("HangulCompatibilityJamo", (0x3130, 0x318F)),
    ("HangulJamo", (0x1100, 0x11FF)),
    ("HangulJamoExtended-A", (0xA960, 0xA97F)),
    ("HangulJamoExtended-B", (0xD7B0, 0xD7FF)),
    ("HangulSyllables", (0xAC00, 0xD7AF)),
    ("HanifiRohingya", (0x10D00, 0x10D3F)),
    ("Hanunoo", (0x1720, 0x173F)),
    ("Hatran", (0x108E0, 0x108FF)),
    ("Hebrew", (0x0590, 0x05FF)),
    ("HighPrivateUseSurrogates", (0xDB80, 0xDBFF)),
    ("HighSurrogates", (0xD800, 0xDB7F)),
    ("Hiragana", (0x3040, 0x309F)),
    ("IPAExtensions", (0x0250, 0x02AF)),
    ("IdeographicDescriptionCharacters", (0x2FF0, 0x2FFF)),
    ("IdeographicSymbolsandPunctuation", (0x16FE0, 0x16FFF)),
    ("ImperialAramaic", (0x10840, 0x1085F)),
    ("IndicSiyaqNumbers", (0x1EC70, 0x1ECBF)),
    ("InscriptionalPahlavi", (0x10B60, 0x10B7F)),
    ("InscriptionalParthian", (0x10B40, 0x10B5F)),
    ("Javanese", (0xA980, 0xA9DF)),
    ("Kaithi", (0x11080, 0x110CF)),
    ("KaktovikNumerals", (0x1D2C0, 0x1D2DF)),
    ("KanaExtended-A", (0x1B100, 0x1B12F)),
    ("KanaExtended-B", (0x1AFF0, 0x1AFFF)),
    ("KanaSupplement", (0x1B000, 0x1B0FF)),
    ("Kanbun", (0x3190, 0x319F)),
    ("KangxiRadicals", (0x2F00, 0x2FDF)),
    ("Kannada", (0x0C80, 0x0CFF)),
    ("Katakana", (0x30A0, 0x30FF)),
    ("KatakanaPhoneticExtensions", (0x31F0, 0x31FF)),
    ("Kawi", (0x11F00, 0x11F5F)),
    ("KayahLi", (0xA900, 0xA92F)),
    ("Kharoshthi", (0x10A00, 0x10A5F)),
    ("KhitanSmallScript", (0x18B00, 0x18CFF)),
    ("Khmer", (0x1780, 0x17FF)),
    ("KhmerSymbols", (0x19E0, 0x19FF)),
    ("Khojki", (0x11200, 0x1124F)),
    ("Khudawadi", (0x112B0, 0x112FF)),
    ("Lao", (0x0E80, 0x0EFF)),
    ("Latin-1Supplement", (0x0080, 0x00FF)),
    ("LatinExtended-A", (0x0100, 0x017F)),
    ("LatinExtended-B", (0x0180, 0x024F)),
    ("LatinExtended-C", (0x2C60, 0x2C7F)),
    ("LatinExtended-D", (0xA720, 0xA7FF)),
    ("LatinExtended-E", (0xAB30, 0xAB6F)),
    ("LatinExtended-F", (0x10780, 0x107BF)),
    ("LatinExtended-G", (0x1DF00, 0x1DFFF)),
    ("LatinExtendedAdditional", (0x1E00, 0x1EFF)),
    ("Lepcha", (0x1C00, 0x1C4F)),
    ("LetterlikeSymbols", (0x2100, 0x214F)),
    ("Limbu", (0x1900, 0x194F)),
    ("LinearA", (0x10600, 0x1077F)),
    ("LinearBIdeograms", (0x10080, 0x100FF)),
    ("LinearBSyllabary", (0x10000, 0x1007F)),
    ("Lisu", (0xA4D0, 0xA4FF)),
    ("LisuSupplement", (0x11FB0, 0x11FBF)),
    ("LowSurrogates", (0xDC00, 0xDFFF)),
    ("Lycian", (0x10280, 0x1029F)),
    ("Lydian", (0x10920, 0x1093F)),
    ("Mahajani", (0x11150, 0x1117F)),
    ("MahjongTiles", (0x1F000, 0x1F02F)),
    ("Makasar", (0x11EE0, 0x11EFF)),
    ("Malayalam", (0x0D00, 0x0D7F)),
    ("Mandaic", (0x0840, 0x085F)),
    ("Manichaean", (0x10AC0, 0x10AFF)),
    ("Marchen", (0x11C70, 0x11CBF)),
    ("MasaramGondi", (0x11D00, 0x11D5F)),
    ("MathematicalAlphanumericSymbols", (0x1D400, 0x1D7FF)),
    ("MathematicalOperators", (0x2200, 0x22FF)),
    ("MayanNumerals", (0x1D2E0, 0x1D2FF)),
    ("Medefaidrin", (0x16E40, 0x16E9F)),
    ("MeeteiMayek", (0xABC0, 0xABFF)),
    ("MeeteiMayekExtensions", (0xAAE0, 0xAAFF)),
    ("MendeKikakui", (0x1E800, 0x1E8DF)),
    ("MeroiticCursive", (0x109A0, 0x109FF)),
    ("MeroiticHieroglyphs", (0x10980, 0x1099F)),
    ("Miao", (0x16F00, 0x16F9F)),
    ("MiscellaneousMathematicalSymbols-A", (0x27C0, 0x27EF)),
    ("MiscellaneousMathematicalSymbols-B", (0x2980, 0x29FF)),
    ("MiscellaneousSymbols", (0x2600, 0x26FF)),
    ("MiscellaneousSymbolsandArrows", (0x2B00, 0x2BFF)),
    ("MiscellaneousSymbolsandPictographs", (0x1F300, 0x1F5FF)),
    ("MiscellaneousTechnical", (0x2300, 0x23FF)),
    ("Modi", (0x11600, 0x1165F)),
    ("ModifierToneLetters", (0xA700, 0xA71F)),
    ("Mongolian", (0x1800, 0x18AF)),
    ("MongolianSupplement", (0x11660, 0x1167F)),
    ("Mro", (0x16A40, 0x16A6F)),
    ("Multani", (0x11280, 0x112AF)),
    ("MusicalSymbols", (0x1D100, 0x1D1FF)),
    ("Myanmar", (0x1000, 0x109F)),
    ("MyanmarExtended-A", (0xAA60, 0xAA7F)),
    ("MyanmarExtended-B", (0xA9E0, 0xA9FF)),
    ("NKo", (0x07C0, 0x07FF)),
    ("Nabataean", (0x10880, 0x108AF)),
    ("NagMundari", (0x1E4D0, 0x1E4FF)),
    ("Nandinagari", (0x119A0, 0x119FF)),
    ("NewTaiLue", (0x1980, 0x19DF)),
    ("Newa", (0x11400, 0x1147F)),
    ("NumberForms", (0x2150, 0x218F)),
    ("Nushu", (0x1B170, 0x1B2FF)),
    ("NyiakengPuachueHmong", (0x1E100, 0x1E14F)),
    ("Ogham", (0x1680, 0x169F)),
    ("OlChiki", (0x1C50, 0x1C7F)),
    ("OldHungarian", (0x10C80, 0x10CFF)),
    ("OldItalic", (0x10300, 0x1032F)),
    ("OldNorthArabian", (0x10A80, 0x10A9F)),
    ("OldPermic", (0x10350, 0x1037F)),
    ("OldPersian", (0x103A0, 0x103DF)),
    ("OldSogdian", (0x10F00, 0x10F2F)),
    ("OldSouthArabian", (0x10A60, 0x10A7F)),
    ("OldTurkic", (0x10C00, 0x10C4F)),
    ("OldUyghur", (0x10F70, 0x10FAF)),
    ("OpticalCharacterRecognition", (0x2440, 0x245F)),
    ("Oriya", (0x0B00, 0x0B7F)),
    ("OrnamentalDingbats", (0x1F650, 0x1F67F)),
    ("Osage", (0x104B0, 0x104FF)),
    ("Osmanya", (0x10480, 0x104AF)),
    ("OttomanSiyaqNumbers", (0x1ED00, 0x1ED4F)),
    ("PahawhHmong", (0x16B00, 0x16B8F)),
    ("Palmyrene", (0x10860, 0x1087F)),
    ("PauCinHau", (0x11AC0, 0x11AFF)),
    ("Phags-pa", (0xA840, 0xA87F)),
    ("PhaistosDisc", (0x101D0, 0x101FF)),
    ("Phoenician", (0x10900, 0x1091F)),
    ("PhoneticExtensions", (0x1D00, 0x1D7F)),
    ("PhoneticExtensionsSupplement", (0x1D80, 0x1DBF)),
    ("PlayingCards", (0x1F0A0, 0x1F0FF)),
    ("PrivateUseArea", (0xE000, 0xF8FF)),
    ("PsalterPahlavi", (0x10B80, 0x10BAF)),
    ("Rejang", (0xA930, 0xA95F)),
    ("RumiNumeralSymbols", (0x10E60, 0x10E7F)),
    ("Runic", (0x16A0, 0x16FF)),
    ("Samaritan", (0x0800, 0x083F)),
    ("Saurashtra", (0xA880, 0xA8DF)),
    ("Sharada", (0x11180, 0x111DF)),
    ("Shavian", (0x10450, 0x1047F)),
    ("ShorthandFormatControls", (0x1BCA0, 0x1BCAF)),
    ("Siddham", (0x11580, 0x115FF)),
    ("Sinhala", (0x0D80, 0x0DFF)),
    ("SinhalaArchaicNumbers", (0x111E0, 0x111FF)),
    ("SmallFormVariants", (0xFE50, 0xFE6F)),
    ("SmallKanaExtension", (0x1B130, 0x1B16F)),
    ("Sogdian", (0x10F30, 0x10F6F)),
    ("SoraSompeng", (0x110D0, 0x110FF)),
    ("Soyombo", (0x11A50, 0x11AAF)),
    ("SpacingModifierLetters", (0x02B0, 0x02FF)),
    ("Specials", (0xFFF0, 0xFFFF)),
    ("Sundanese", (0x1B80, 0x1BBF)),
    ("SundaneseSupplement", (0x1CC0, 0x1CCF)),
    ("SuperscriptsandSubscripts", (0x2070, 0x209F)),
    ("SupplementalArrows-A", (0x27F0, 0x27FF)),
    ("SupplementalArrows-B", (0x2900, 0x297F)),
    ("SupplementalArrows-C", (0x1F800, 0x1F8FF)),
    ("SupplementalMathematicalOperators", (0x2A00, 0x2AFF)),
    ("SupplementalPunctuation", (0x2E00, 0x2E7F)),
    ("SupplementalSymbolsandPictographs", (0x1F900, 0x1F9FF)),
    ("SupplementaryPrivateUseArea-A", (0xF0000, 0xFFFFF)),
    ("SupplementaryPrivateUseArea-B", (0x100000, 0x10FFFF)),
    ("SuttonSignWriting", (0x1D800, 0x1DAAF)),
    ("SylotiNagri", (0xA800, 0xA82F)),
    ("SymbolsandPictographsExtended-A", (0x1FA70, 0x1FAFF)),
    ("SymbolsforLegacyComputing", (0x1FB00, 0x1FBFF)),
    ("Syriac", (0x0700, 0x074F)),
    ("SyriacSupplement", (0x0860, 0x086F)),
    ("Tagalog", (0x1700, 0x171F)),
    ("Tagbanwa", (0x1760, 0x177F)),
    ("Tags", (0xE0000, 0xE007F)),
    ("TaiLe", (0x1950, 0x197F)),
    ("TaiTham", (0x1A20, 0x1AAF)),
    ("TaiViet", (0xAA80, 0xAADF)),
    ("TaiXuanJingSymbols", (0x1D300, 0x1D35F)),
    ("Takri", (0x11680, 0x116CF)),
    ("Tamil", (0x0B80, 0x0BFF)),
    ("TamilSupplement", (0x11FC0, 0x11FFF)),
    ("Tangsa", (0x16A70, 0x16ACF)),
    ("Tangut", (0x17000, 0x187FF)),
    ("TangutComponents", (0x18800, 0x18AFF)),
    ("TangutSupplement", (0x18D00, 0x18D7F)),
    ("Telugu", (0x0C00, 0x0C7F)),
    ("Thaana", (0x0780, 0x07BF)),
    ("Thai", (0x0E00, 0x0E7F)),
    ("Tibetan", (0x0F00, 0x0FFF)),
    ("Tifinagh", (0x2D30, 0x2D7F)),
    ("Tirhuta", (0x11480, 0x114DF)),
    ("Toto", (0x1E290, 0x1E2BF)),
    ("TransportandMapSymbols", (0x1F680, 0x1F6FF)),
    ("Ugaritic", (0x10380, 0x1039F)),
    ("UnifiedCanadianAboriginalSyllabics", (0x1400, 0x167F)),
    (
        "UnifiedCanadianAboriginalSyllabicsExtended",
        (0x18B0, 0x18FF),
    ),
    (
        "UnifiedCanadianAboriginalSyllabicsExtended-A",
        (0x11AB0, 0x11ABF),
    ),
    ("Vai", (0xA500, 0xA63F)),
    ("VariationSelectors", (0xFE00, 0xFE0F)),
    ("VariationSelectorsSupplement", (0xE0100, 0xE01EF)),
    ("VedicExtensions", (0x1CD0, 0x1CFF)),
    ("VerticalForms", (0xFE10, 0xFE1F)),
    ("Vithkuqi", (0x10570, 0x105BF)),
    ("Wancho", (0x1E2C0, 0x1E2FF)),
    ("WarangCiti", (0x118A0, 0x118FF)),
    ("Yezidi", (0x10E80, 0x10EBF)),
    ("YiRadicals", (0xA490, 0xA4CF)),
    ("YiSyllables", (0xA000, 0xA48F)),
    ("YijingHexagramSymbols", (0x4DC0, 0x4DFF)),
    ("ZanabazarSquare", (0x11A00, 0x11A4F)),
    ("ZnamennyMusicalNotation", (0x1CF00, 0x1CFCF)),
];

/// `IsPrivateUse` under XSD 1.0: the BMP Private Use Area only. The planes
/// 15 and 16 areas were not in Unicode 3.0, and the W3C MS reL/reM/reN tests
/// require them to be excluded under XSD 1.0 (as the `xsd11` build does, see
/// `xsd10_private_use_block_body`).
const PRIVATE_USE_XSD10: &[(u32, u32)] = &[(0xE000, 0xF8FF)];

/// `IsPrivateUse` under XSD 1.1, as listed among the superseded Unicode 3.1
/// names in 1.1 §G.4.2.3: "#xE000 - #xF8FF: PrivateUse", "#xF0000 - #xFFFFD:
/// PrivateUse", "#x100000 - #x10FFFD: PrivateUse".
const PRIVATE_USE_XSD11: &[(u32, u32)] =
    &[(0xE000, 0xF8FF), (0xF0000, 0xFFFFD), (0x100000, 0x10FFFD)];

/// The code-point ranges of the block named `name` (the `X` of `\p{IsX}`),
/// or `None` when `name` is not a recognized block name.
pub(crate) fn block_ranges(name: &str, xsd_version: XsdVersion) -> Option<&'static [(u32, u32)]> {
    if name == "PrivateUse" {
        return Some(match xsd_version {
            XsdVersion::V1_0 => PRIVATE_USE_XSD10,
            XsdVersion::V1_1 => PRIVATE_USE_XSD11,
        });
    }
    BLOCKS
        .binary_search_by(|(block, _)| (*block).cmp(name))
        .ok()
        .map(|index| std::slice::from_ref(&BLOCKS[index].1))
}

/// Append `ranges` to `out` as the body of a character class (what goes
/// between `[` and `]`), in the style of the category bodies: each end point
/// as its literal character, with the class metacharacters `[`, `\`, `]`,
/// `-`, `^` (and the `regex` set operators' `&`, `~`) escaped. Surrogate code
/// points, which no string holds and the `regex` crate rejects, are cut from
/// the ends of a range; a range across them keeps its ends, since splitting
/// it at the gap would trip the `regex` crate (0.8 syntax), whose negation of
/// a class holding both U+D7FF and U+E000 yields the inverted gap
/// U+D7FF..=U+E000. Returns `false` when nothing was appended, i.e. the
/// ranges hold no character (the surrogate blocks).
pub(crate) fn push_ranges_body(out: &mut String, ranges: &[(u32, u32)]) -> bool {
    const SURROGATES: std::ops::RangeInclusive<u32> = 0xD800..=0xDFFF;
    let mut any = false;
    for &(start, end) in ranges {
        let start = if SURROGATES.contains(&start) {
            0xE000
        } else {
            start
        };
        let end = if SURROGATES.contains(&end) {
            0xD7FF
        } else {
            end
        };
        if start > end {
            continue;
        }
        push_class_char(out, start);
        if end > start {
            out.push('-');
            push_class_char(out, end);
        }
        any = true;
    }
    any
}

fn push_class_char(out: &mut String, cp: u32) {
    let Some(c) = char::from_u32(cp) else {
        return;
    };
    if matches!(c, '[' | '\\' | ']' | '-' | '^' | '&' | '~') {
        out.push('\\');
    }
    out.push(c);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_well_formed() {
        for pair in BLOCKS.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} / {}", pair[0].0, pair[1].0);
        }
        for &(name, (start, end)) in BLOCKS {
            assert!(start <= end && end <= 0x10FFFF, "{name}");
            assert!(
                name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "{name} is not an IsBlock name"
            );
        }
    }

    #[test]
    fn lookups() {
        let v10 = XsdVersion::V1_0;
        let v11 = XsdVersion::V1_1;
        assert_eq!(block_ranges("BasicLatin", v10), Some(&[(0x0, 0x7F)][..]));
        assert_eq!(
            block_ranges("Latin-1Supplement", v10),
            Some(&[(0x80, 0xFF)][..])
        );
        assert_eq!(
            block_ranges("Greek", v10),
            block_ranges("GreekandCoptic", v10)
        );
        assert_eq!(block_ranges("PrivateUse", v10), Some(PRIVATE_USE_XSD10));
        assert_eq!(block_ranges("PrivateUse", v11), Some(PRIVATE_USE_XSD11));
        // Exact names only: no case folding, no dropped hyphens.
        for name in [
            "basicLatin",
            "Latin1Supplement",
            "Basic Latin",
            "",
            "NoSuchBlock",
        ] {
            assert_eq!(block_ranges(name, v11), None, "{name}");
        }
    }

    #[test]
    fn bodies() {
        let body = |ranges: &[(u32, u32)]| {
            let mut out = String::new();
            push_ranges_body(&mut out, ranges).then_some(out)
        };
        assert_eq!(body(&[(0x41, 0x5A)]).as_deref(), Some("A-Z"));
        assert_eq!(body(&[(0x2D, 0x2D)]).as_deref(), Some(r"\-"));
        assert_eq!(body(&[(0x5B, 0x5E)]).as_deref(), Some(r"\[-\^"));
        // Surrogates are cut from the ends; a block of nothing else is empty,
        // and a range across them is kept whole.
        assert_eq!(body(&[(0xD800, 0xDB7F)]), None);
        assert_eq!(
            body(&[(0xD700, 0xDBFF), (0xDC00, 0xE001)]).as_deref(),
            Some("\u{D700}-\u{D7FF}\u{E000}-\u{E001}")
        );
        assert_eq!(
            body(&[(0x0, 0x10FFFF)]).as_deref(),
            Some("\u{0}-\u{10FFFF}")
        );
    }

    /// The table means what `regexml` (the `xsd11` engine) means: every name
    /// is a block there, and its first and last code points are in it while
    /// the ones just outside are not.
    #[cfg(feature = "xsd11")]
    #[test]
    fn blocks_agree_with_regexml() {
        let is_char = |cp: u32| char::from_u32(cp).is_some();
        for &(name, (start, end)) in BLOCKS {
            let regex = regexml::Regex::xsd(&format!(r"\p{{Is{name}}}"), "")
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let matches = |cp: u32| regex.is_full_match(&char::from_u32(cp).unwrap().to_string());
            for cp in [start, end] {
                if is_char(cp) {
                    assert!(matches(cp), "{name}: U+{cp:04X} should be in it");
                }
            }
            for cp in [start.wrapping_sub(1), end + 1] {
                if cp <= 0x10FFFF && is_char(cp) {
                    assert!(!matches(cp), "{name}: U+{cp:04X} should not be in it");
                }
            }
        }
        for (name, ranges) in [("PrivateUse", PRIVATE_USE_XSD11)] {
            let regex = regexml::Regex::xsd(&format!(r"\p{{Is{name}}}"), "").unwrap();
            for &(start, end) in ranges {
                for cp in [start, end] {
                    assert!(regex.is_full_match(&char::from_u32(cp).unwrap().to_string()));
                }
            }
        }
    }
}
