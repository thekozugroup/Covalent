package fixture

internal class SafTransferBridgeFixture {
    fun issueBackup(node: Node, baseUrl: String, token: String) {
        node.openConnection(baseUrl, "/api/v1/backups/archive", "GET", token)
    }

    fun issueRestore(node: Node, baseUrl: String, token: String) {
        node.openConnection(baseUrl, "/api/v1/restores/archive/execute", "POST", token)
    }
}
