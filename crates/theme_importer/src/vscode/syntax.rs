use indexmap::IndexMap;
use serde::Deserialize;
use strum::EnumIter;

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum VsCodeTokenScope {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize)]
pub struct VsCodeTokenColor {
    pub name: Option<String>,
    pub scope: Option<VsCodeTokenScope>,
    pub settings: VsCodeTokenColorSettings,
}

#[derive(Debug, Deserialize)]
pub struct VsCodeTokenColorSettings {
    pub foreground: Option<String>,
    pub background: Option<String>,
    #[serde(rename = "fontStyle")]
    pub font_style: Option<String>,
}

#[derive(Debug, PartialEq, Copy, Clone, EnumIter)]
pub enum FlintSyntaxToken {
    Attribute,
    Boolean,
    Comment,
    CommentDoc,
    Constant,
    Constructor,
    Embedded,
    Emphasis,
    EmphasisStrong,
    Enum,
    Function,
    Hint,
    Keyword,
    Label,
    LinkText,
    LinkUri,
    Number,
    Operator,
    Predictive,
    Preproc,
    Primary,
    Property,
    Punctuation,
    PunctuationBracket,
    PunctuationDelimiter,
    PunctuationListMarker,
    PunctuationSpecial,
    String,
    StringEscape,
    StringRegex,
    StringSpecial,
    StringSpecialSymbol,
    Tag,
    TextLiteral,
    Title,
    Type,
    Variable,
    VariableSpecial,
    Variant,
}

impl std::fmt::Display for FlintSyntaxToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                FlintSyntaxToken::Attribute => "attribute",
                FlintSyntaxToken::Boolean => "boolean",
                FlintSyntaxToken::Comment => "comment",
                FlintSyntaxToken::CommentDoc => "comment.doc",
                FlintSyntaxToken::Constant => "constant",
                FlintSyntaxToken::Constructor => "constructor",
                FlintSyntaxToken::Embedded => "embedded",
                FlintSyntaxToken::Emphasis => "emphasis",
                FlintSyntaxToken::EmphasisStrong => "emphasis.strong",
                FlintSyntaxToken::Enum => "enum",
                FlintSyntaxToken::Function => "function",
                FlintSyntaxToken::Hint => "hint",
                FlintSyntaxToken::Keyword => "keyword",
                FlintSyntaxToken::Label => "label",
                FlintSyntaxToken::LinkText => "link_text",
                FlintSyntaxToken::LinkUri => "link_uri",
                FlintSyntaxToken::Number => "number",
                FlintSyntaxToken::Operator => "operator",
                FlintSyntaxToken::Predictive => "predictive",
                FlintSyntaxToken::Preproc => "preproc",
                FlintSyntaxToken::Primary => "primary",
                FlintSyntaxToken::Property => "property",
                FlintSyntaxToken::Punctuation => "punctuation",
                FlintSyntaxToken::PunctuationBracket => "punctuation.bracket",
                FlintSyntaxToken::PunctuationDelimiter => "punctuation.delimiter",
                FlintSyntaxToken::PunctuationListMarker => "punctuation.list_marker",
                FlintSyntaxToken::PunctuationSpecial => "punctuation.special",
                FlintSyntaxToken::String => "string",
                FlintSyntaxToken::StringEscape => "string.escape",
                FlintSyntaxToken::StringRegex => "string.regex",
                FlintSyntaxToken::StringSpecial => "string.special",
                FlintSyntaxToken::StringSpecialSymbol => "string.special.symbol",
                FlintSyntaxToken::Tag => "tag",
                FlintSyntaxToken::TextLiteral => "text.literal",
                FlintSyntaxToken::Title => "title",
                FlintSyntaxToken::Type => "type",
                FlintSyntaxToken::Variable => "variable",
                FlintSyntaxToken::VariableSpecial => "variable.special",
                FlintSyntaxToken::Variant => "variant",
            }
        )
    }
}

impl FlintSyntaxToken {
    pub fn find_best_token_color_match<'a>(
        &self,
        token_colors: &'a [VsCodeTokenColor],
    ) -> Option<&'a VsCodeTokenColor> {
        let mut ranked_matches = IndexMap::new();

        for (ix, token_color) in token_colors.iter().enumerate() {
            if token_color.settings.foreground.is_none() {
                continue;
            }

            let Some(rank) = self.rank_match(token_color) else {
                continue;
            };

            if rank > 0 {
                ranked_matches.insert(ix, rank);
            }
        }

        ranked_matches
            .into_iter()
            .max_by_key(|(_, rank)| *rank)
            .map(|(ix, _)| &token_colors[ix])
    }

    fn rank_match(&self, token_color: &VsCodeTokenColor) -> Option<u32> {
        let candidate_scopes = match token_color.scope.as_ref()? {
            VsCodeTokenScope::One(scope) => vec![scope],
            VsCodeTokenScope::Many(scopes) => scopes.iter().collect(),
        }
        .iter()
        .flat_map(|scope| scope.split(',').map(|s| s.trim()))
        .collect::<Vec<_>>();

        let scopes_to_match = self.to_vscode();
        let number_of_scopes_to_match = scopes_to_match.len();

        let mut matches = 0;

        for (ix, scope) in scopes_to_match.into_iter().enumerate() {
            // Assign each entry a weight that is inversely proportional to its
            // position in the list.
            //
            // Entries towards the front are weighted higher than those towards the end.
            let weight = (number_of_scopes_to_match - ix) as u32;

            if candidate_scopes.contains(&scope) {
                matches += 1 + weight;
            }
        }

        Some(matches)
    }

    pub fn fallbacks(&self) -> &[Self] {
        match self {
            FlintSyntaxToken::CommentDoc => &[FlintSyntaxToken::Comment],
            FlintSyntaxToken::Number => &[FlintSyntaxToken::Constant],
            FlintSyntaxToken::VariableSpecial => &[FlintSyntaxToken::Variable],
            FlintSyntaxToken::PunctuationBracket
            | FlintSyntaxToken::PunctuationDelimiter
            | FlintSyntaxToken::PunctuationListMarker
            | FlintSyntaxToken::PunctuationSpecial => &[FlintSyntaxToken::Punctuation],
            FlintSyntaxToken::StringEscape
            | FlintSyntaxToken::StringRegex
            | FlintSyntaxToken::StringSpecial
            | FlintSyntaxToken::StringSpecialSymbol => &[FlintSyntaxToken::String],
            _ => &[],
        }
    }

    fn to_vscode(self) -> Vec<&'static str> {
        match self {
            FlintSyntaxToken::Attribute => vec!["entity.other.attribute-name"],
            FlintSyntaxToken::Boolean => vec!["constant.language"],
            FlintSyntaxToken::Comment => vec!["comment"],
            FlintSyntaxToken::CommentDoc => vec!["comment.block.documentation"],
            FlintSyntaxToken::Constant => {
                vec!["constant", "constant.language", "constant.character"]
            }
            FlintSyntaxToken::Constructor => {
                vec![
                    "entity.name.tag",
                    "entity.name.function.definition.special.constructor",
                ]
            }
            FlintSyntaxToken::Embedded => vec!["meta.embedded"],
            FlintSyntaxToken::Emphasis => vec!["markup.italic"],
            FlintSyntaxToken::EmphasisStrong => vec![
                "markup.bold",
                "markup.italic markup.bold",
                "markup.bold markup.italic",
            ],
            FlintSyntaxToken::Enum => vec!["support.type.enum"],
            FlintSyntaxToken::Function => vec![
                "entity.function",
                "entity.name.function",
                "variable.function",
            ],
            FlintSyntaxToken::Hint => vec![],
            FlintSyntaxToken::Keyword => vec![
                "keyword",
                "keyword.other.fn.rust",
                "keyword.control",
                "keyword.control.fun",
                "keyword.control.class",
                "punctuation.accessor",
                "entity.name.tag",
            ],
            FlintSyntaxToken::Label => vec![
                "label",
                "entity.name",
                "entity.name.import",
                "entity.name.package",
            ],
            FlintSyntaxToken::LinkText => vec!["markup.underline.link", "string.other.link"],
            FlintSyntaxToken::LinkUri => vec!["markup.underline.link", "string.other.link"],
            FlintSyntaxToken::Number => vec!["constant.numeric", "number"],
            FlintSyntaxToken::Operator => vec!["operator", "keyword.operator"],
            FlintSyntaxToken::Predictive => vec![],
            FlintSyntaxToken::Preproc => vec![
                "preproc",
                "meta.preprocessor",
                "punctuation.definition.preprocessor",
            ],
            FlintSyntaxToken::Primary => vec![],
            FlintSyntaxToken::Property => vec![
                "variable.member",
                "support.type.property-name",
                "variable.object.property",
                "variable.other.field",
            ],
            FlintSyntaxToken::Punctuation => vec![
                "punctuation",
                "punctuation.section",
                "punctuation.accessor",
                "punctuation.separator",
                "punctuation.definition.tag",
            ],
            FlintSyntaxToken::PunctuationBracket => vec![
                "punctuation.bracket",
                "punctuation.definition.tag.begin",
                "punctuation.definition.tag.end",
            ],
            FlintSyntaxToken::PunctuationDelimiter => vec![
                "punctuation.delimiter",
                "punctuation.separator",
                "punctuation.terminator",
            ],
            FlintSyntaxToken::PunctuationListMarker => {
                vec!["markup.list punctuation.definition.list.begin"]
            }
            FlintSyntaxToken::PunctuationSpecial => vec!["punctuation.special"],
            FlintSyntaxToken::String => vec!["string"],
            FlintSyntaxToken::StringEscape => {
                vec!["string.escape", "constant.character", "constant.other"]
            }
            FlintSyntaxToken::StringRegex => vec!["string.regex"],
            FlintSyntaxToken::StringSpecial => vec!["string.special", "constant.other.symbol"],
            FlintSyntaxToken::StringSpecialSymbol => {
                vec!["string.special.symbol", "constant.other.symbol"]
            }
            FlintSyntaxToken::Tag => vec!["tag", "entity.name.tag", "meta.tag.sgml"],
            FlintSyntaxToken::TextLiteral => vec!["text.literal", "string"],
            FlintSyntaxToken::Title => vec!["title", "entity.name"],
            FlintSyntaxToken::Type => vec![
                "entity.name.type",
                "entity.name.type.primitive",
                "entity.name.type.numeric",
                "keyword.type",
                "support.type",
                "support.type.primitive",
                "support.class",
            ],
            FlintSyntaxToken::Variable => vec![
                "variable",
                "variable.language",
                "variable.member",
                "variable.parameter",
                "variable.parameter.function-call",
            ],
            FlintSyntaxToken::VariableSpecial => vec![
                "variable.special",
                "variable.member",
                "variable.annotation",
                "variable.language",
            ],
            FlintSyntaxToken::Variant => vec!["variant"],
        }
    }
}
