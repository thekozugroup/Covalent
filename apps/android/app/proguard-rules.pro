-keepattributes Signature,InnerClasses,EnclosingMethod

# JNI_OnLoad registers the complete fixed ABI, including nativeState even when
# no Kotlin release call site uses it. Removing one method rejects registration.
-keep class life.michaelwong.covalent.node.CovalentNative {
    native <methods>;
}
