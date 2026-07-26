# LLM Providers

The Chat feature requires an LLM (Large Language Model) provider to power the natural language interface. Conductor supports multiple providers, and you can switch between them at any time.

## Supported Providers

| Provider | Models | Best For |
|----------|--------|----------|
| **OpenAI** | GPT-4o, GPT-4o-mini | Fast responses, broad knowledge |
| **Anthropic** | Claude 3.5 Sonnet | Detailed explanations, careful reasoning |

## Setting Up a Provider

### OpenAI

1. Go to [platform.openai.com](https://platform.openai.com)
2. Sign in or create an account
3. Navigate to **API Keys** in the sidebar
4. Click **Create new secret key**
5. Copy the key (starts with `sk-`)

In Conductor:
1. Open **Settings** (gear icon)
2. Go to the **Chat** tab
3. Select **OpenAI** as your provider
4. Paste your API key
5. Click **Save**

### Anthropic

1. Go to [console.anthropic.com](https://console.anthropic.com)
2. Sign in or create an account
3. Navigate to **API Keys**
4. Click **Create Key**
5. Copy the key (starts with `sk-ant-`)

In Conductor:
1. Open **Settings** (gear icon)
2. Go to the **Chat** tab
3. Select **Anthropic** as your provider
4. Paste your API key
5. Click **Save**

## API Key Security

Your API keys are stored securely in your system's keychain:

- **macOS**: Stored in Keychain Access
- **Windows**: Stored in Windows Credential Manager
- **Linux**: Stored in the Secret Service (e.g., GNOME Keyring)

Keys are never:
- Stored in plain text files
- Logged to console or files
- Sent anywhere except the provider's API

## Checking Provider Status

You can verify your provider is configured correctly:

1. Open the **Chat** panel
2. Look for the provider indicator at the bottom
3. A green dot means the provider is connected
4. A red dot means there's a configuration issue

## Switching Providers

You can change providers at any time:

1. Open **Settings** > **Chat**
2. Select a different provider
3. Enter the API key if not already configured
4. Click **Save**

Your conversation history is preserved when switching providers.

## Cost Considerations

Both providers charge per token (roughly 4 characters = 1 token):

| Provider | Input Cost | Output Cost |
|----------|------------|-------------|
| OpenAI GPT-4o | ~$5/1M tokens | ~$15/1M tokens |
| OpenAI GPT-4o-mini | ~$0.15/1M tokens | ~$0.60/1M tokens |
| Anthropic Claude 3.5 Sonnet | ~$3/1M tokens | ~$15/1M tokens |

Typical Conductor chat sessions use 500-2000 tokens per message, so costs are minimal for normal usage.

## Troubleshooting

### "API key not configured"

1. Open Settings > Chat
2. Verify your API key is entered correctly
3. Check that you selected the correct provider

### "Authentication failed"

1. Your API key may be invalid or expired
2. Generate a new key from your provider's dashboard
3. Update the key in Conductor settings

### "Rate limit exceeded"

1. You've made too many requests too quickly
2. Wait a few seconds and try again
3. Consider upgrading your API tier for higher limits

### "Network error"

1. Check your internet connection
2. Verify no firewall is blocking API access
3. Try again in a few moments

## Environment Variables (Advanced)

For automation or CI/CD, you can set API keys via environment variables:

```bash
# OpenAI
export OPENAI_API_KEY="sk-..."

# Anthropic
export ANTHROPIC_API_KEY="sk-ant-..."
```

Environment variables take precedence over keychain-stored keys.

## MIDI Learn Integration (v4.26.55+)

All providers support automatic MIDI Learn recommendations. When you request a mapping without specifying the exact note or CC number, the assistant will proactively suggest using MIDI Learn to capture the correct control from your device. See [Chat Interface - MIDI Learn Integration](chat.md#midi-learn-integration-v42655) for details.

## Next Steps

- [Chat Interface](chat.md) - Learn how to use the Chat feature
- [Troubleshooting Chat](../troubleshooting/chat-issues.md) - Common issues
