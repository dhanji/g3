import React, { useState, useEffect } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import StatusBadge from '../components/StatusBadge'
import ChatView from '../components/ChatView'
import GitStatus from '../components/GitStatus'
import ProgressBar from '../components/ProgressBar'

function Detail() {
  const { id } = useParams()
  const navigate = useNavigate()
  const [instance, setInstance] = useState(null)
  const [logs, setLogs] = useState({ messages: [], tool_calls: [] })
  const [loading, setLoading] = useState(true)

  const fetchInstance = async () => {
    try {
      const response = await fetch(`/api/instances/${id}`)
      if (response.ok) {
        const data = await response.json()
        setInstance(data)
      }
    } catch (error) {
      console.error('Failed to fetch instance:', error)
    }
  }

  const fetchLogs = async () => {
    try {
      const response = await fetch(`/api/instances/${id}/logs`)
      if (response.ok) {
        const data = await response.json()
        setLogs(data)
      }
    } catch (error) {
      console.error('Failed to fetch logs:', error)
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    fetchInstance()
    fetchLogs()
    const interval = setInterval(() => {
      fetchInstance()
      fetchLogs()
    }, 5000)
    return () => clearInterval(interval)
  }, [id])

  if (loading || !instance) {
    return (
      <div className="flex justify-center items-center h-64">
        <div className="text-gray-600 dark:text-gray-400">Loading instance details...</div>
      </div>
    )
  }

  return (
    <div>
      <button
        onClick={() => navigate('/')}
        className="mb-4 text-blue-600 hover:text-blue-800 dark:text-blue-400 dark:hover:text-blue-300"
      >
        ← Back to instances
      </button>

      {/* Summary Section */}
      <div className="hero-card p-6 mb-6">
        <div className="flex justify-between items-start mb-4">
          <div>
            <h2 className="text-2xl font-bold text-gray-900 dark:text-white mb-2">
              Instance {instance.instance.id}
            </h2>
            <div className="flex items-center gap-2">
              <StatusBadge status={instance.instance.status} />
              <span className="text-sm text-gray-600 dark:text-gray-400">
                {instance.instance.instance_type === 'ensemble' ? 'Coach + Player' : 'Single Agent'}
              </span>
            </div>
          </div>
        </div>

        <ProgressBar
          instanceType={instance.instance.instance_type}
          durationSecs={instance.stats.duration_secs}
        />

        <div className="grid grid-cols-3 gap-4 mt-4">
          <div>
            <div className="text-sm text-gray-600 dark:text-gray-400">Tokens</div>
            <div className="text-2xl font-bold text-gray-900 dark:text-white">
              {instance.stats.total_tokens.toLocaleString()}
            </div>
          </div>
          <div>
            <div className="text-sm text-gray-600 dark:text-gray-400">Tool Calls</div>
            <div className="text-2xl font-bold text-gray-900 dark:text-white">
              {instance.stats.tool_calls}
            </div>
          </div>
          <div>
            <div className="text-sm text-gray-600 dark:text-gray-400">Errors</div>
            <div className="text-2xl font-bold text-gray-900 dark:text-white">
              {instance.stats.errors}
            </div>
          </div>
        </div>

        <div className="mt-4 text-sm text-gray-600 dark:text-gray-400">
          <div><strong>Workspace:</strong> {instance.instance.workspace}</div>
          <div><strong>Provider:</strong> {instance.instance.provider || 'N/A'}</div>
          <div><strong>Model:</strong> {instance.instance.model || 'N/A'}</div>
          <div><strong>Started:</strong> {new Date(instance.instance.start_time).toLocaleString()}</div>
        </div>
      </div>

      {/* Project Context Section */}
      <div className="hero-card p-6 mb-6">
        <h3 className="text-xl font-bold text-gray-900 dark:text-white mb-4">Project Context</h3>
        
        {/* Project Files */}
        <div className="space-y-4">
          {instance.project_files.requirements && (
            <div>
              <h4 className="font-semibold text-gray-900 dark:text-white mb-2">requirements.md</h4>
              <pre className="text-sm text-gray-700 dark:text-gray-300 whitespace-pre-wrap">
                {instance.project_files.requirements}
              </pre>
            </div>
          )}
          {instance.project_files.readme && (
            <div>
              <h4 className="font-semibold text-gray-900 dark:text-white mb-2">README.md</h4>
              <pre className="text-sm text-gray-700 dark:text-gray-300 whitespace-pre-wrap">
                {instance.project_files.readme}
              </pre>
            </div>
          )}
          {instance.project_files.agents && (
            <div>
              <h4 className="font-semibold text-gray-900 dark:text-white mb-2">AGENTS.md</h4>
              <pre className="text-sm text-gray-700 dark:text-gray-300 whitespace-pre-wrap">
                {instance.project_files.agents}
              </pre>
            </div>
          )}
        </div>

        {/* Git Status */}
        {instance.git_status && (
          <div className="mt-6">
            <GitStatus status={instance.git_status} />
          </div>
        )}
      </div>

      {/* Chat View Section */}
      <div className="hero-card p-6">
        <h3 className="text-xl font-bold text-gray-900 dark:text-white mb-4">Chat History</h3>
        <ChatView messages={logs.messages} toolCalls={logs.tool_calls} />
      </div>
    </div>
  )
}

export default Detail
