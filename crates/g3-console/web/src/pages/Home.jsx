import React, { useState, useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import InstancePanel from '../components/InstancePanel'
import NewRunModal from '../components/NewRunModal'

function Home() {
  const [instances, setInstances] = useState([])
  const [loading, setLoading] = useState(true)
  const [showModal, setShowModal] = useState(false)
  const navigate = useNavigate()

  const fetchInstances = async () => {
    try {
      const response = await fetch('/api/instances')
      if (response.ok) {
        const data = await response.json()
        setInstances(data)
      }
    } catch (error) {
      console.error('Failed to fetch instances:', error)
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    fetchInstances()
    const interval = setInterval(fetchInstances, 5000) // Poll every 5 seconds
    return () => clearInterval(interval)
  }, [])

  const handleInstanceClick = (id) => {
    navigate(`/instance/${id}`)
  }

  const handleKill = async (id) => {
    try {
      const response = await fetch(`/api/instances/${id}/kill`, {
        method: 'POST',
      })
      if (response.ok) {
        fetchInstances()
      }
    } catch (error) {
      console.error('Failed to kill instance:', error)
    }
  }

  const handleRestart = async (id) => {
    try {
      const response = await fetch(`/api/instances/${id}/restart`, {
        method: 'POST',
      })
      if (response.ok) {
        fetchInstances()
      }
    } catch (error) {
      console.error('Failed to restart instance:', error)
    }
  }

  const handleLaunch = async (request) => {
    try {
      const response = await fetch('/api/instances/launch', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify(request),
      })
      if (response.ok) {
        setShowModal(false)
        setTimeout(fetchInstances, 2000) // Refresh after 2 seconds
      }
    } catch (error) {
      console.error('Failed to launch instance:', error)
    }
  }

  if (loading) {
    return (
      <div className="flex justify-center items-center h-64">
        <div className="text-gray-600 dark:text-gray-400">Loading instances...</div>
      </div>
    )
  }

  return (
    <div>
      <div className="flex justify-between items-center mb-6">
        <h2 className="text-xl font-semibold text-gray-900 dark:text-white">
          Running Instances ({instances.length})
        </h2>
        <button
          onClick={() => setShowModal(true)}
          className="hero-button hero-button-primary"
        >
          + New Run
        </button>
      </div>

      {instances.length === 0 ? (
        <div className="hero-card p-8 text-center">
          <p className="text-gray-600 dark:text-gray-400">
            No running instances. Click "New Run" to start a g3 instance.
          </p>
        </div>
      ) : (
        <div className="space-y-4">
          {instances.map((instance) => (
            <InstancePanel
              key={instance.instance.id}
              instance={instance}
              onClick={() => handleInstanceClick(instance.instance.id)}
              onKill={() => handleKill(instance.instance.id)}
              onRestart={() => handleRestart(instance.instance.id)}
            />
          ))}
        </div>
      )}

      {showModal && (
        <NewRunModal
          onClose={() => setShowModal(false)}
          onLaunch={handleLaunch}
        />
      )}
    </div>
  )
}

export default Home
