package life.michaelwong.covalent.appliance

import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.model.NodeConnection

/** The screen and background reader use the same authenticated local-node routes. */
internal class NodeApplianceApi(
    private val connection: () -> NodeConnection?,
    private val client: CovalentNodeClient = CovalentNodeClient(),
) : ApplianceDashboardApi, ApplianceImportsApi, ApplianceUpdatesApi {
    private fun ready() = checkNotNull(connection())
    override fun context() = ready().let { client.applianceContext(it.baseUrl, it.token) }
    override fun pairing() = context()
    override fun names() = ready().let { client.peerGrants(it.baseUrl, it.token) }
        .filterNot { it.revoked }.associate { it.peerDeviceId to it.displayName }
    override fun status(peerId: String, localId: String) = ready().let { client.applianceStatus(it.baseUrl, it.token, peerId, localId) }
    override fun platform(peerId: String, localId: String) = ready().let { client.appliancePlatform(it.baseUrl, it.token, peerId, localId) }
    override fun context(selected: SelectedAppliance) = ready().let { client.applianceImports(it.baseUrl, it.token, selected) }
    override fun actions(local: ApplianceIdentity) = ready().let { client.applianceImportActions(it.baseUrl, it.token, local) }
    override fun confirm(choice: PiImportChoice) = ready().let { client.confirmApplianceImport(it.baseUrl, it.token, choice) }
    override fun check(action: PiSavedImport) = ready().let { client.checkApplianceImport(it.baseUrl, it.token, action) }
    override fun withdraw(action: PiSavedImport) = ready().let { client.withdrawApplianceImport(it.baseUrl, it.token, action) }
    override fun updateContext(selected: SelectedAppliance) = ready().let { client.applianceUpdates(it.baseUrl, it.token, selected) }
    override fun updateActions(local: ApplianceIdentity) = ready().let { client.applianceUpdateActions(it.baseUrl, it.token, local) }
    override fun confirmUpdate(choice: PiUpdateChoice) = ready().let { client.confirmApplianceUpdate(it.baseUrl, it.token, choice) }
    override fun checkUpdate(action: PiSavedUpdate) = ready().let { client.checkApplianceUpdate(it.baseUrl, it.token, action) }
}
