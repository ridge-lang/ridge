package lang.ridge.intellij

import com.intellij.openapi.editor.colors.TextAttributesKey

/**
 * Colour keys for the semantic-token kinds the stock IntelliJ schemes leave at
 * the default foreground (types, parameters, call-site functions). The actual
 * colours are registered per scheme through `additionalTextAttributes` in
 * plugin.xml — the same mechanism LSP4IJ uses for its own keys, and the one its
 * highlighter reads via the colour scheme.
 */
object RidgeColors {
    val TYPE: TextAttributesKey = TextAttributesKey.createTextAttributesKey("RIDGE_TYPE")
    val PARAMETER: TextAttributesKey = TextAttributesKey.createTextAttributesKey("RIDGE_PARAMETER")
    val FUNCTION: TextAttributesKey = TextAttributesKey.createTextAttributesKey("RIDGE_FUNCTION")
}
