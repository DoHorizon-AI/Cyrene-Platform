package com.cy.plugin.jvm;

import java.io.InputStream;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import cy.plugin.v1.PluginProtocol.Envelope;
import cy.plugin.v1.PluginProtocol.HelloAck;
import cy.plugin.v1.PluginProtocol.InvokeResult;
import cy.plugin.v1.Notification.SendNotificationResponse;
import cy.plugin.v1.PluginProtocol.PluginErrorPayload;
import cy.plugin.v1.ExecutionEngine.ExecuteInferenceResponse;

/**
 * Minimal Java/JVM Plugin POC implementation over zero-port stdio binary framing protocol.
 * This class handles standard framing (4-byte big-endian header) and routes messages.
 */
public class PocNotificationPlugin {

    public static void main(String[] args) {
        System.err.println("[jvm_plugin_poc] Starting JVM Notification Plugin POC over stdio...");

        InputStream stdin = System.in;
        OutputStream stdout = System.out;

        try {
            byte[] header = new byte[4];
            while (true) {
                int readBytes = readFully(stdin, header, 4);
                if (readBytes < 4) {
                    System.err.println("[jvm_plugin_poc] EOF or stream closed, exiting.");
                    break;
                }

                ByteBuffer bb = ByteBuffer.wrap(header);
                bb.order(ByteOrder.BIG_ENDIAN);
                int payloadLen = bb.getInt();

                if (payloadLen <= 0 || payloadLen > 64 * 1024 * 1024) {
                    System.err.println("[jvm_plugin_poc] Invalid frame length: " + payloadLen);
                    System.exit(1);
                }

                byte[] payload = new byte[payloadLen];
                int n = readFully(stdin, payload, payloadLen);
                if (n < payloadLen) {
                    System.err.println("[jvm_plugin_poc] EOF while reading payload");
                    break;
                }

                Envelope reqEnv = Envelope.parseFrom(payload);

                if (reqEnv.hasHello()) {
                    System.err.println("[jvm_plugin_poc] Received Hello, sending HelloAck");
                    Envelope respEnv = Envelope.newBuilder()
                        .setRequestId(reqEnv.getRequestId())
                        .setTraceId(reqEnv.getTraceId())
                        .setPluginId(reqEnv.getPluginId())
                        .setProtocolVersion(reqEnv.getProtocolVersion())
                        .setHelloAck(HelloAck.newBuilder()
                            .setSelectedProtocolVersion(reqEnv.getHello().getMaxProtocolVersion())
                            .setPluginId(reqEnv.getPluginId())
                            .setPluginVersion("1.0.0")
                            .setApiVersion("1.0")
                            .addDeclaredCapabilities("notification:jvm")
                            .build())
                        .build();
                    sendEnvelope(stdout, respEnv);
                } else if (reqEnv.hasInvoke()) {
                    String method = reqEnv.getInvoke().getMethod();
                    System.err.println("[jvm_plugin_poc] Received Invoke for method: " + method);

                    if ("send_notification".equals(method)) {
                        Envelope respEnv = Envelope.newBuilder()
                            .setRequestId(reqEnv.getRequestId())
                            .setTraceId(reqEnv.getTraceId())
                            .setPluginId(reqEnv.getPluginId())
                            .setProtocolVersion(reqEnv.getProtocolVersion())
                            .setInvokeResult(InvokeResult.newBuilder()
                                .setSendNotification(SendNotificationResponse.newBuilder().build())
                                .build())
                            .build();
                        sendEnvelope(stdout, respEnv);
                    } else if ("execute_inference".equals(method)) {
                        Envelope respEnv = Envelope.newBuilder()
                            .setRequestId(reqEnv.getRequestId())
                            .setTraceId(reqEnv.getTraceId())
                            .setPluginId(reqEnv.getPluginId())
                            .setProtocolVersion(reqEnv.getProtocolVersion())
                            .setInvokeResult(InvokeResult.newBuilder()
                                .setExecuteInference(ExecuteInferenceResponse.newBuilder().setOutputText("jvm-ok").build())
                                .build())
                            .build();
                        sendEnvelope(stdout, respEnv);
                    } else {
                        Envelope respEnv = Envelope.newBuilder()
                            .setRequestId(reqEnv.getRequestId())
                            .setTraceId(reqEnv.getTraceId())
                            .setPluginId(reqEnv.getPluginId())
                            .setProtocolVersion(reqEnv.getProtocolVersion())
                            .setError(PluginErrorPayload.newBuilder()
                                .setCode(PluginErrorPayload.Code.UNAVAILABLE)
                                .setMessage("Method not implemented")
                                .build())
                            .build();
                        sendEnvelope(stdout, respEnv);
                    }
                } else if (reqEnv.hasShutdown()) {
                    System.err.println("[jvm_plugin_poc] Received Shutdown");
                    System.exit(0);
                }
            }
        } catch (Exception e) {
            System.err.println("[jvm_plugin_poc] Exception: " + e.getMessage());
            e.printStackTrace(System.err);
            System.exit(1);
        }
    }

    private static int readFully(InputStream in, byte[] b, int len) throws java.io.IOException {
        int n = 0;
        while (n < len) {
            int count = in.read(b, n, len - n);
            if (count < 0) {
                break;
            }
            n += count;
        }
        return n;
    }

    private static void sendEnvelope(OutputStream out, Envelope env) throws java.io.IOException {
        byte[] payload = env.toByteArray();
        ByteBuffer bb = ByteBuffer.allocate(4);
        bb.order(ByteOrder.BIG_ENDIAN);
        bb.putInt(payload.length);
        out.write(bb.array());
        out.write(payload);
        out.flush();
    }
}
