export type TracerState = {
  project: {
    budget: number
    primer: boolean
    enrich: boolean
    projectDocs: boolean
  }
}

export const initialState: TracerState = {
  project: {
    budget: 10_000,
    primer: true,
    enrich: true,
    projectDocs: true,
  },
}
