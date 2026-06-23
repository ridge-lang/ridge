package lang.ridge.intellij

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.PersistentStateComponent
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage

/**
 * Holds the optional override for the `ridge-lsp` binary location, mirroring the
 * VS Code `ridge.lspPath` setting: leave it empty to auto-resolve, or set an
 * absolute path when the binary lives off the resolver's chain.
 */
@Service(Service.Level.APP)
@State(name = "RidgeSettings", storages = [Storage("ridge.xml")])
class RidgeSettings : PersistentStateComponent<RidgeSettings.State> {
    class State {
        @JvmField
        var lspPath: String = ""
    }

    private var state = State()

    override fun getState(): State = state

    override fun loadState(state: State) {
        this.state = state
    }

    var lspPath: String
        get() = state.lspPath
        set(value) {
            state.lspPath = value
        }

    companion object {
        fun getInstance(): RidgeSettings =
            ApplicationManager.getApplication().getService(RidgeSettings::class.java)
    }
}
