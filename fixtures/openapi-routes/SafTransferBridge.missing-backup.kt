package fixture

internal class SafTransferBridgeFixture {
    fun issueRestore(node: Node, baseUrl: String, token: String) {
        node.openConnection(baseUrl, "/api/v1/restores/archive/execute", "POST", token)
    }
}
