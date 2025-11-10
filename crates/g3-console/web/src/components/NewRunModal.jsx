import React, { useState } from 'react'

function NewRunModal({ onClose, onLaunch }) {
  const [prompt, setPrompt] = useState('')
  const [workspace, setWorkspace] = useState('')
  const [provider, setProvider] = useState('databricks')
  const [model, setModel] = useState('databricks-claude-sonnet-4-5')
  const [mode, setMode] = useState('single')
  const [g3BinaryPath, setG3BinaryPath] = useState('')
  const [loading, setLoading] = useState(false)

  const handleSubmit = async (e) => {
    e.preventDefault()
    setLoading(true)

    const request = {
      prompt,
      workspace,
      provider,
      model,
      mode,
      g3_binary_path: g3BinaryPath || null,
    }

    await onLaunch(request)
    setLoading(false)
  }

  const isValid = prompt.trim() && workspace.trim()

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className="hero-card p-6 max-w-2xl w-full max-h-[90vh] overflow-y-auto">
        <h2 className="text-2xl font-bold text-gray-900 dark:text-white mb-4">
          New Run
        </h2>

        <form onSubmit={handleSubmit} className="space-y-4">
          <div>
            <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
              Initial Prompt *
            </label>
            <textarea
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder="Describe what you want g3 to build..."
              className="hero-input"
              rows={4}
              required
            />
          </div>

          <div>
            <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
              Workspace Directory *
            </label>
            <input
              type="text"
              value={workspace}
              onChange={(e) => setWorkspace(e.target.value)}
              placeholder="/path/to/workspace"
              className="hero-input"
              required
            />
          </div>

          <div>
            <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
              G3 Binary Path (optional)
            </label>
            <input
              type="text"
              value={g3BinaryPath}
              onChange={(e) => setG3BinaryPath(e.target.value)}
              placeholder="g3 (default) or /path/to/g3"
              className="hero-input"
            />
          </div>

          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                Provider
              </label>
              <select
                value={provider}
                onChange={(e) => setProvider(e.target.value)}
                className="hero-input"
              >
                <option value="databricks">Databricks</option>
                <option value="anthropic">Anthropic</option>
                <option value="local">Local</option>
              </select>
            </div>

            <div>
              <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                Model
              </label>
              <select
                value={model}
                onChange={(e) => setModel(e.target.value)}
                className="hero-input"
              >
                {provider === 'databricks' && (
                  <>
                    <option value="databricks-claude-sonnet-4-5">Claude Sonnet 4.5</option>
                    <option value="databricks-meta-llama-3-1-405b-instruct">Llama 3.1 405B</option>
                  </>
                )}
                {provider === 'anthropic' && (
                  <>
                    <option value="claude-3-5-sonnet-20241022">Claude 3.5 Sonnet</option>
                    <option value="claude-3-opus-20240229">Claude 3 Opus</option>
                  </>
                )}
                {provider === 'local' && (
                  <option value="local-model">Local Model</option>
                )}
              </select>
            </div>
          </div>

          <div>
            <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-2">
              Execution Mode
            </label>
            <div className="space-y-2">
              <label className="flex items-center">
                <input
                  type="radio"
                  value="single"
                  checked={mode === 'single'}
                  onChange={(e) => setMode(e.target.value)}
                  className="mr-2"
                />
                <span className="text-gray-700 dark:text-gray-300">
                  Single-shot (one agent, one task)
                </span>
              </label>
              <label className="flex items-center">
                <input
                  type="radio"
                  value="ensemble"
                  checked={mode === 'ensemble'}
                  onChange={(e) => setMode(e.target.value)}
                  className="mr-2"
                />
                <span className="text-gray-700 dark:text-gray-300">
                  Coach + Player Ensemble (autonomous mode)
                </span>
              </label>
            </div>
          </div>

          <div className="flex justify-end gap-2 pt-4">
            <button
              type="button"
              onClick={onClose}
              className="hero-button hero-button-secondary"
              disabled={loading}
            >
              Cancel
            </button>
            <button
              type="submit"
              className="hero-button hero-button-primary"
              disabled={!isValid || loading}
            >
              {loading ? 'Starting...' : 'Start'}
            </button>
          </div>
        </form>
      </div>
    </div>
  )
}

export default NewRunModal
